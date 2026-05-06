//! Pipeline validation — integration tests for multi-service data/control plane.
//!
//! Validates the kernel's capability model by simulating a two-service
//! pipeline: an OS service writes a scene graph to a shared VMO and signals
//! a "frame ready" event, then a compositor reads the scene graph.
//!
//! Tests exercise shared memory, event signaling, handle rights enforcement,
//! and PeerClosed detection at the kernel API level.

#[cfg(test)]
mod tests {
    use crate::{
        address_space::AddressSpace,
        bootstrap, config,
        endpoint::Endpoint,
        event::Event,
        frame::state,
        thread::Thread,
        types::{
            AddressSpaceId, EndpointId, EventId, ObjectType, Priority, Rights, ThreadId, VmoId,
        },
        vmo::{Vmo, VmoFlags},
    };

    struct TwoServiceSetup {
        _svc_thread: ThreadId,
        comp_thread: ThreadId,
        svc_space: AddressSpaceId,
        comp_space: AddressSpaceId,
        shared_vmo: VmoId,
        event: EventId,
        endpoint: EndpointId,
    }

    fn setup_two_services() -> TwoServiceSetup {
        crate::frame::arch::page_table::reset_asid_pool();

        state::init(2);

        let svc_space = AddressSpace::new(AddressSpaceId(0), 1, 0);
        let (svc_idx, _) = state::spaces().alloc_shared(svc_space).unwrap();

        state::spaces().write(svc_idx).unwrap().id = AddressSpaceId(svc_idx);

        let comp_space = AddressSpace::new(AddressSpaceId(0), 2, 0);
        let (comp_idx, _) = state::spaces().alloc_shared(comp_space).unwrap();

        state::spaces().write(comp_idx).unwrap().id = AddressSpaceId(comp_idx);

        let svc_thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(svc_idx)),
            Priority::Medium,
            0x1000,
            0x2000,
            0,
        );
        let (svc_tid, _) = state::threads().alloc_shared(svc_thread).unwrap();

        state::threads().write(svc_tid).unwrap().id = ThreadId(svc_tid);
        state::threads()
            .write(svc_tid)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        state::inc_alive_threads();
        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(svc_tid)));

        let comp_thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(comp_idx)),
            Priority::Medium,
            0x1000,
            0x2000,
            0,
        );
        let (comp_tid, _) = state::threads().alloc_shared(comp_thread).unwrap();

        state::threads().write(comp_tid).unwrap().id = ThreadId(comp_tid);
        state::inc_alive_threads();
        state::schedulers()
            .core(1)
            .lock()
            .enqueue(ThreadId(comp_tid), Priority::Medium);

        let shared_vmo = Vmo::new(VmoId(0), config::PAGE_SIZE * 4, VmoFlags::NONE);
        let (vmo_idx, _) = state::vmos().alloc_shared(shared_vmo).unwrap();

        state::vmos().write(vmo_idx).unwrap().id = VmoId(vmo_idx);

        let event = Event::new(EventId(0));
        let (evt_idx, _) = state::events().alloc_shared(event).unwrap();

        state::events().write(evt_idx).unwrap().id = EventId(evt_idx);

        let endpoint = Endpoint::new(EndpointId(0));
        let (ep_idx, _) = state::endpoints().alloc_shared(endpoint).unwrap();

        state::endpoints().write(ep_idx).unwrap().id = EndpointId(ep_idx);

        TwoServiceSetup {
            _svc_thread: ThreadId(svc_tid),
            comp_thread: ThreadId(comp_tid),
            svc_space: AddressSpaceId(svc_idx),
            comp_space: AddressSpaceId(comp_idx),
            shared_vmo: VmoId(vmo_idx),
            event: EventId(evt_idx),
            endpoint: EndpointId(ep_idx),
        }
    }

    // -- Shared memory (data plane) --

    #[test]
    fn shared_vmo_mapped_in_both_spaces() {
        let s = setup_two_services();
        let vmo_size = config::PAGE_SIZE * 4;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let ro = Rights::READ;
        let svc_va = state::spaces()
            .write(s.svc_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, rw, 0)
            .unwrap();
        let comp_va = state::spaces()
            .write(s.comp_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, ro, 0)
            .unwrap();

        assert!(svc_va > 0);
        assert!(comp_va > 0);

        let svc_space = state::spaces().read(s.svc_space.0).unwrap();
        let svc_mapping = svc_space.find_mapping(svc_va).unwrap();

        assert_eq!(svc_mapping.vmo_id, s.shared_vmo);
        assert!(svc_mapping.rights.contains(Rights::WRITE));

        drop(svc_space);

        let comp_space = state::spaces().read(s.comp_space.0).unwrap();
        let comp_mapping = comp_space.find_mapping(comp_va).unwrap();

        assert_eq!(comp_mapping.vmo_id, s.shared_vmo);
        assert!(!comp_mapping.rights.contains(Rights::WRITE));

        drop(comp_space);

        crate::invariants::assert_valid();
    }

    // -- Event signaling (control plane) --

    #[test]
    fn event_signal_wakes_compositor() {
        let s = setup_two_services();
        let mut event = state::events().write(s.event.0).unwrap();

        event.add_waiter(s.comp_thread, 0b1).unwrap();

        let woken = event.signal(0b1);

        assert_eq!(woken.len(), 1);
        assert_eq!(woken.as_slice()[0].thread_id, s.comp_thread);
        assert_eq!(woken.as_slice()[0].fired_bits, 0b1);

        drop(event);

        crate::invariants::assert_valid();
    }

    #[test]
    fn multi_frame_event_cycle() {
        let s = setup_two_services();

        for frame in 0..10 {
            let mut event = state::events().write(s.event.0).unwrap();

            event.add_waiter(s.comp_thread, 0b1).unwrap();

            let woken = event.signal(0b1);

            assert_eq!(woken.len(), 1, "frame {frame}: compositor not woken");

            event.clear(0b1);

            assert!(
                event.check(0b1).is_none(),
                "frame {frame}: bits not cleared"
            );
        }

        crate::invariants::assert_valid();
    }

    // -- Handle rights enforcement --

    #[test]
    fn handle_rights_attenuate_on_dup() {
        let s = setup_two_services();
        let vmo_gen = state::vmos().generation(s.shared_vmo.0);

        // Maintain refcount when installing handles outside the syscall layer.
        state::vmos().write(s.shared_vmo.0).unwrap().add_ref();

        let mut svc_space = state::spaces().write(s.svc_space.0).unwrap();
        let full_hid = svc_space
            .handles_mut()
            .allocate(ObjectType::Vmo, s.shared_vmo.0, Rights::ALL, vmo_gen)
            .unwrap();

        drop(svc_space);

        state::vmos().write(s.shared_vmo.0).unwrap().add_ref();

        let mut svc_space = state::spaces().write(s.svc_space.0).unwrap();
        let read_only_hid = svc_space
            .handles_mut()
            .duplicate(full_hid, Rights::READ)
            .unwrap();
        let dup_handle = svc_space.handles().lookup(read_only_hid).unwrap();

        assert!(dup_handle.rights.contains(Rights::READ));
        assert!(!dup_handle.rights.contains(Rights::WRITE));

        drop(svc_space);

        crate::invariants::assert_valid();
    }

    // -- Endpoint peer closed --

    #[test]
    fn endpoint_peer_closed_unblocks() {
        let s = setup_two_services();
        let mut ep = state::endpoints().write(s.endpoint.0).unwrap();

        ep.add_recv_waiter(s.comp_thread).unwrap();

        assert_eq!(ep.recv_waiter_count(), 1);

        let result = ep.close_peer().unwrap();
        let all_ids: alloc::vec::Vec<_> = result.all_thread_ids().collect();

        assert!(all_ids.contains(&s.comp_thread));
        assert!(ep.is_peer_closed());

        drop(ep);

        crate::invariants::assert_valid();
    }

    // -- VMO snapshot (COW for undo) --

    #[test]
    fn snapshot_creates_independent_copy() {
        let s = setup_two_services();
        let snap = state::vmos()
            .read(s.shared_vmo.0)
            .unwrap()
            .snapshot(VmoId(0));
        let (snap_idx, _) = state::vmos().alloc_shared(snap).unwrap();

        state::vmos().write(snap_idx).unwrap().id = VmoId(snap_idx);

        assert_eq!(state::vmos().count(), 2);

        let snap_vmo = state::vmos().read(snap_idx).unwrap();

        assert_eq!(snap_vmo.size(), config::PAGE_SIZE * 4);
        assert_eq!(snap_vmo.cow_parent(), Some(s.shared_vmo));

        drop(snap_vmo);

        crate::invariants::assert_valid();
    }

    // -- Bootstrap integration --

    #[test]
    fn bootstrap_creates_schedulable_init() {
        crate::frame::arch::page_table::reset_asid_pool();

        state::init(2);

        let tid = bootstrap::create_init(&[0u8; 100]).unwrap();
        let thread = state::threads().read(tid.0).unwrap();

        assert!(thread.entry_point() >= config::PAGE_SIZE);
        assert!(thread.entry_point().is_multiple_of(config::PAGE_SIZE));
        assert!(thread.stack_top() > config::PAGE_SIZE);
        assert!(thread.stack_top().is_multiple_of(config::PAGE_SIZE));
        assert_eq!(
            thread.state(),
            crate::thread::ThreadRunState::Running,
            "init should be marked Running"
        );

        drop(thread);

        assert_eq!(
            state::schedulers().core(0).lock().current(),
            Some(tid),
            "init should be set as current on core 0"
        );

        crate::invariants::assert_valid();
    }

    // -- Full pipeline validation --

    #[test]
    fn pipeline_data_control_plane_10_frames() {
        let s = setup_two_services();
        let vmo_size = config::PAGE_SIZE * 4;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let ro = Rights::READ;

        state::spaces()
            .write(s.svc_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, rw, 0)
            .unwrap();
        state::spaces()
            .write(s.comp_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, ro, 0)
            .unwrap();

        for frame in 0..10 {
            let mut event = state::events().write(s.event.0).unwrap();

            event.add_waiter(s.comp_thread, 0b1).unwrap();

            let woken = event.signal(0b1);

            assert_eq!(woken.len(), 1, "frame {frame}");
            assert_eq!(woken.as_slice()[0].thread_id, s.comp_thread);

            event.clear(0b1);
        }

        crate::invariants::assert_valid();
    }
}
