//! Host-vs-bare-metal differential testing.
//!
//! Defines canonical syscall sequences that must produce identical results
//! on both the host target (via `dispatch()`) and bare-metal (via
//! real SVC calls in the hypervisor). Any divergence indicates a `#[cfg]`
//! bug where the host stub and real implementation disagree.
//!
//! The expected results generated here are compiled into the bare-metal
//! integration test binary for comparison.

#[cfg(test)]
mod tests {
    use crate::{
        address_space::AddressSpace,
        config,
        frame::state,
        syscall::num,
        thread::Thread,
        types::{AddressSpaceId, HandleId, ObjectType, Priority, Rights, SyscallError, ThreadId},
    };

    fn setup() {
        crate::frame::arch::page_table::reset_asid_pool();

        state::init(1);

        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        state::spaces().alloc_shared(space);

        let thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            Priority::Medium,
            0,
            0,
            0,
        );

        state::threads().alloc_shared(thread);
        state::threads()
            .write(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        state::inc_alive_threads();
        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(0)));
    }

    fn call(num: u64, args: &[u64; 6]) -> (u64, u64) {
        let space_id = crate::syscall::thread_space_id(ThreadId(0)).ok();

        crate::syscall::dispatch(ThreadId(0), space_id, 0, num, args)
    }

    fn inv() {
        crate::invariants::assert_valid();
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
        setup();

        // Create VMO → (0, handle_id)
        let (e, vmo) = call(num::VMO_CREATE, &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Info on VMO → (0, packed_type_rights)
        let (e, info) = call(num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        let obj_type = (info >> 32) as u8;
        let rights = (info & 0xFFFF_FFFF) as u32;

        assert_eq!(obj_type, ObjectType::Vmo as u8);
        assert_eq!(rights, Rights::ALL.0);

        // Dup with read-only → (0, dup_handle_id)
        let (e, dup) = call(num::HANDLE_DUP, &[vmo, Rights::READ.0 as u64, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_ne!(dup, vmo);

        // Info on dup → read-only rights
        let (e, info) = call(num::HANDLE_INFO, &[dup, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        let dup_rights = (info & 0xFFFF_FFFF) as u32;

        assert_eq!(dup_rights, Rights::READ.0);

        // Close dup → (0, 0)
        let (e, _) = call(num::HANDLE_CLOSE, &[dup, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Info on closed dup → InvalidHandle
        let (e, _) = call(num::HANDLE_INFO, &[dup, 0, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::InvalidHandle as u64);

        // Original still valid
        let (e, _) = call(num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Close original
        let (e, _) = call(num::HANDLE_CLOSE, &[vmo, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        inv();
    }

    #[test]
    fn diff_event_signal_clear_check() {
        setup();

        let (e, evt) = call(num::EVENT_CREATE, &[0; 6]);

        assert_eq!(e, 0);

        // Signal bits 0x5
        let (e, _) = call(num::EVENT_SIGNAL, &[evt, 0x5, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Wait for bit 0x4 — should succeed immediately
        let (e, fired) = call(num::EVENT_WAIT, &[evt, 0x4, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!(fired, evt);

        // Clear bit 0x1
        let (e, _) = call(num::EVENT_CLEAR, &[evt, 0x1, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Wait for bit 0x1 — should block (bit was cleared)
        let (e, _) = call(num::EVENT_WAIT, &[evt, 0x1, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!(
            state::threads().read(0).unwrap().state(),
            crate::thread::ThreadRunState::Blocked
        );

        // Restore thread: Blocked → Ready → Running
        state::threads()
            .write(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Ready);
        state::threads()
            .write(0)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(0)));

        inv();
    }

    #[test]
    fn diff_endpoint_lifecycle() {
        setup();

        let (e, ep) = call(num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(e, 0);

        let (e, info) = call(num::HANDLE_INFO, &[ep, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        let obj_type = (info >> 32) as u8;

        assert_eq!(obj_type, ObjectType::Endpoint as u8);

        // Create event and bind
        let (e, evt) = call(num::EVENT_CREATE, &[0; 6]);

        assert_eq!(e, 0);

        let (e, _) = call(num::ENDPOINT_BIND_EVENT, &[ep, evt, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Close both
        let (e, _) = call(num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        let (e, _) = call(num::HANDLE_CLOSE, &[evt, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        inv();
    }

    #[test]
    fn diff_system_info_constants() {
        setup();

        // Page size
        let (e, val) = call(num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!(val, config::PAGE_SIZE as u64);

        // Message size
        let (e, val) = call(num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!(val, crate::endpoint::MSG_SIZE as u64);

        // Core count
        let (e, val) = call(num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert!(val >= 1);

        // Invalid info type
        let (e, _) = call(num::SYSTEM_INFO, &[3, 0, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::InvalidArgument as u64);

        let (e, _) = call(num::SYSTEM_INFO, &[u64::MAX, 0, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::InvalidArgument as u64);

        inv();
    }

    #[test]
    fn diff_error_codes_match() {
        setup();

        // Invalid syscall → InvalidArgument
        let (e, _) = call(34, &[0; 6]);

        assert_eq!(e, SyscallError::InvalidArgument as u64);

        let (e, _) = call(u64::MAX, &[0; 6]);

        assert_eq!(e, SyscallError::InvalidArgument as u64);

        // Invalid handle → InvalidHandle
        let (e, _) = call(num::HANDLE_INFO, &[999, 0, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::InvalidHandle as u64);

        // Wrong handle type — use VMO handle for endpoint operation
        let (_, vmo) = call(num::VMO_CREATE, &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0]);
        let (e, _) = call(num::EVENT_SIGNAL, &[vmo, 0x1, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::WrongHandleType as u64);

        // VMO create with zero size → InvalidArgument
        let (e, _) = call(num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(e, SyscallError::InvalidArgument as u64);

        // VMO seal then resize → AlreadySealed
        let (e, _) = call(num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        let (e, _) = call(
            num::VMO_RESIZE,
            &[vmo, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
        );

        assert_eq!(e, SyscallError::AlreadySealed as u64);

        // Dup with rights escalation → InsufficientRights
        let (_, read_only) = call(num::HANDLE_DUP, &[vmo, Rights::READ.0 as u64, 0, 0, 0, 0]);
        let (e, _) = call(
            num::HANDLE_DUP,
            &[read_only, Rights::ALL.0 as u64, 0, 0, 0, 0],
        );

        assert_eq!(e, SyscallError::InsufficientRights as u64);

        inv();
    }

    #[test]
    fn diff_vmo_snapshot_seal_resize() {
        setup();

        let (e, vmo) = call(num::VMO_CREATE, &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Snapshot
        let (e, snap) = call(num::VMO_SNAPSHOT, &[vmo, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Snapshot info shows VMO type
        let (e, info) = call(num::HANDLE_INFO, &[snap, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!((info >> 32) as u8, ObjectType::Vmo as u8);

        // Resize original
        let (e, _) = call(
            num::VMO_RESIZE,
            &[vmo, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
        );

        assert_eq!(e, 0);

        // Seal snapshot — should succeed
        let (e, _) = call(num::VMO_SEAL, &[snap, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Resize snapshot — fails (sealed)
        let (e, _) = call(
            num::VMO_RESIZE,
            &[snap, config::PAGE_SIZE as u64, 0, 0, 0, 0],
        );

        assert_eq!(e, SyscallError::AlreadySealed as u64);

        inv();
    }

    #[test]
    fn diff_ipc_call_blocks_without_receiver() {
        setup();

        let (e, ep) = call(num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(e, 0);

        // Call with empty message — should block (no receiver)
        let mut buf = [0u8; 128];
        let (e, _) = call(num::CALL, &[ep, buf.as_mut_ptr() as u64, 0, 0, 0, 0]);

        assert_eq!(e, 0);
        assert_eq!(
            state::threads().read(0).unwrap().state(),
            crate::thread::ThreadRunState::Blocked
        );

        // Thread is blocked on the endpoint — verify the pending call exists
        let pending = state::endpoints()
            .read(
                state::spaces()
                    .read(0)
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
        setup();

        let page = config::PAGE_SIZE as u64;
        // Create two VMOs so we have handles at slots 0 and 1
        let (_, h1) = call(num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);
        let (_, h2) = call(num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);

        assert_ne!(h1, h2);

        // Close h1, create new — should reuse h1's slot
        call(num::HANDLE_CLOSE, &[h1, 0, 0, 0, 0, 0]);

        let (e, h3) = call(num::VMO_CREATE, &[page, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // h2 is still valid (unaffected by h1's close/reuse)
        let (e, _) = call(num::HANDLE_INFO, &[h2, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // h3 is valid
        let (e, _) = call(num::HANDLE_INFO, &[h3, 0, 0, 0, 0, 0]);

        assert_eq!(e, 0);

        // Double close returns InvalidHandle
        let (e, _) = call(num::HANDLE_CLOSE, &[h1, 0, 0, 0, 0, 0]);

        // May succeed (slot reused to h3) or fail — depends on whether h1 == h3
        if h1 == h3 {
            assert_eq!(e, 0);
        } else {
            assert_eq!(e, SyscallError::InvalidHandle as u64);
        }

        inv();
    }
}
