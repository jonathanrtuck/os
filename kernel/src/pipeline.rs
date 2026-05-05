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
    use alloc::boxed::Box;

    use crate::{
        address_space::AddressSpace,
        bootstrap, config,
        endpoint::Endpoint,
        event::Event,
        syscall::Kernel,
        thread::Thread,
        types::{
            AddressSpaceId, EndpointId, EventId, ObjectType, Priority, Rights, ThreadId, VmoId,
        },
        vmo::{Vmo, VmoFlags},
    };

    struct TwoServiceSetup {
        kernel: Box<Kernel>,
        _svc_thread: ThreadId,
        comp_thread: ThreadId,
        svc_space: AddressSpaceId,
        comp_space: AddressSpaceId,
        shared_vmo: VmoId,
        event: EventId,
        endpoint: EndpointId,
    }

    fn setup_two_services() -> TwoServiceSetup {
        let mut k = Box::new(Kernel::new(2));
        let svc_space = AddressSpace::new(AddressSpaceId(0), 1, 0);
        let (svc_idx, _) = k.spaces.alloc(svc_space).unwrap();

        k.spaces.get_mut(svc_idx).unwrap().id = AddressSpaceId(svc_idx);

        let comp_space = AddressSpace::new(AddressSpaceId(0), 2, 0);
        let (comp_idx, _) = k.spaces.alloc(comp_space).unwrap();

        k.spaces.get_mut(comp_idx).unwrap().id = AddressSpaceId(comp_idx);

        let svc_thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(svc_idx)),
            Priority::Medium,
            0x1000,
            0x2000,
            0,
        );
        let (svc_tid, _) = k.threads.alloc(svc_thread).unwrap();

        k.threads.get_mut(svc_tid).unwrap().id = ThreadId(svc_tid);
        k.threads
            .get_mut(svc_tid)
            .unwrap()
            .set_state(crate::thread::ThreadRunState::Running);
        k.scheduler.core_mut(0).set_current(Some(ThreadId(svc_tid)));

        let comp_thread = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(comp_idx)),
            Priority::Medium,
            0x1000,
            0x2000,
            0,
        );
        let (comp_tid, _) = k.threads.alloc(comp_thread).unwrap();

        k.threads.get_mut(comp_tid).unwrap().id = ThreadId(comp_tid);
        k.scheduler.enqueue(1, ThreadId(comp_tid), Priority::Medium);

        let shared_vmo = Vmo::new(VmoId(0), config::PAGE_SIZE * 4, VmoFlags::NONE);
        let (vmo_idx, _) = k.vmos.alloc(shared_vmo).unwrap();

        k.vmos.get_mut(vmo_idx).unwrap().id = VmoId(vmo_idx);

        let event = Event::new(EventId(0));
        let (evt_idx, _) = k.events.alloc(event).unwrap();

        k.events.get_mut(evt_idx).unwrap().id = EventId(evt_idx);

        let endpoint = Endpoint::new(EndpointId(0));
        let (ep_idx, _) = k.endpoints.alloc(endpoint).unwrap();

        k.endpoints.get_mut(ep_idx).unwrap().id = EndpointId(ep_idx);

        TwoServiceSetup {
            kernel: k,
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
        let mut s = setup_two_services();
        let vmo_size = config::PAGE_SIZE * 4;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let ro = Rights::READ;
        let svc_va = s
            .kernel
            .spaces
            .get_mut(s.svc_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, rw, 0)
            .unwrap();
        let comp_va = s
            .kernel
            .spaces
            .get_mut(s.comp_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, ro, 0)
            .unwrap();

        assert!(svc_va > 0);
        assert!(comp_va > 0);

        let svc_mapping = s
            .kernel
            .spaces
            .get(s.svc_space.0)
            .unwrap()
            .find_mapping(svc_va)
            .unwrap();

        assert_eq!(svc_mapping.vmo_id, s.shared_vmo);
        assert!(svc_mapping.rights.contains(Rights::WRITE));

        let comp_mapping = s
            .kernel
            .spaces
            .get(s.comp_space.0)
            .unwrap()
            .find_mapping(comp_va)
            .unwrap();

        assert_eq!(comp_mapping.vmo_id, s.shared_vmo);
        assert!(!comp_mapping.rights.contains(Rights::WRITE));

        crate::invariants::assert_valid(&*s.kernel);
    }

    // -- Event signaling (control plane) --

    #[test]
    fn event_signal_wakes_compositor() {
        let mut s = setup_two_services();
        let event = s.kernel.events.get_mut(s.event.0).unwrap();

        event.add_waiter(s.comp_thread, 0b1).unwrap();

        let woken = event.signal(0b1);

        assert_eq!(woken.len(), 1);
        assert_eq!(woken.as_slice()[0].thread_id, s.comp_thread);
        assert_eq!(woken.as_slice()[0].fired_bits, 0b1);

        crate::invariants::assert_valid(&*s.kernel);
    }

    #[test]
    fn multi_frame_event_cycle() {
        let mut s = setup_two_services();

        for frame in 0..10 {
            let event = s.kernel.events.get_mut(s.event.0).unwrap();

            event.add_waiter(s.comp_thread, 0b1).unwrap();

            let woken = event.signal(0b1);

            assert_eq!(woken.len(), 1, "frame {frame}: compositor not woken");

            event.clear(0b1);

            assert!(
                event.check(0b1).is_none(),
                "frame {frame}: bits not cleared"
            );
        }

        crate::invariants::assert_valid(&*s.kernel);
    }

    // -- Handle rights enforcement --

    #[test]
    fn handle_rights_attenuate_on_dup() {
        let mut s = setup_two_services();
        let vmo_gen = s.kernel.vmos.generation(s.shared_vmo.0);
        let svc_space = s.kernel.spaces.get_mut(s.svc_space.0).unwrap();
        let full_hid = svc_space
            .handles_mut()
            .allocate(ObjectType::Vmo, s.shared_vmo.0, Rights::ALL, vmo_gen)
            .unwrap();
        let read_only_hid = svc_space
            .handles_mut()
            .duplicate(full_hid, Rights::READ)
            .unwrap();
        let dup_handle = svc_space.handles().lookup(read_only_hid).unwrap();

        assert!(dup_handle.rights.contains(Rights::READ));
        assert!(!dup_handle.rights.contains(Rights::WRITE));

        crate::invariants::assert_valid(&*s.kernel);
    }

    // -- Endpoint peer closed --

    #[test]
    fn endpoint_peer_closed_unblocks() {
        let mut s = setup_two_services();
        let ep = s.kernel.endpoints.get_mut(s.endpoint.0).unwrap();

        ep.add_recv_waiter(s.comp_thread).unwrap();

        assert_eq!(ep.recv_waiter_count(), 1);

        let result = ep.close_peer();
        let all_ids: alloc::vec::Vec<_> = result.all_thread_ids().collect();

        assert!(all_ids.contains(&s.comp_thread));
        assert!(ep.is_peer_closed());

        crate::invariants::assert_valid(&*s.kernel);
    }

    // -- VMO snapshot (COW for undo) --

    #[test]
    fn snapshot_creates_independent_copy() {
        let mut s = setup_two_services();
        let parent = s.kernel.vmos.get(s.shared_vmo.0).unwrap();
        let snap = parent.snapshot(VmoId(0));
        let (snap_idx, _) = s.kernel.vmos.alloc(snap).unwrap();

        s.kernel.vmos.get_mut(snap_idx).unwrap().id = VmoId(snap_idx);

        assert_eq!(s.kernel.vmos.count(), 2);

        let snap_vmo = s.kernel.vmos.get(snap_idx).unwrap();

        assert_eq!(snap_vmo.size(), config::PAGE_SIZE * 4);
        assert_eq!(snap_vmo.cow_parent(), Some(s.shared_vmo));

        crate::invariants::assert_valid(&*s.kernel);
    }

    // -- Bootstrap integration --

    #[test]
    fn bootstrap_creates_schedulable_init() {
        let mut k = Box::new(Kernel::new(2));
        let tid = bootstrap::create_init(&mut k, &[0u8; 100]).unwrap();
        let thread = k.threads.get(tid.0).unwrap();

        assert_eq!(thread.entry_point(), bootstrap::INIT_CODE_VA);
        assert_eq!(
            thread.stack_top(),
            bootstrap::INIT_STACK_VA + bootstrap::INIT_STACK_SIZE
        );
        assert_eq!(
            thread.state(),
            crate::thread::ThreadRunState::Running,
            "init should be marked Running"
        );
        assert_eq!(
            k.scheduler.core(0).current(),
            Some(tid),
            "init should be set as current on core 0"
        );

        crate::invariants::assert_valid(&*k);
    }

    // -- Full pipeline validation --

    #[test]
    fn pipeline_data_control_plane_10_frames() {
        let mut s = setup_two_services();
        let vmo_size = config::PAGE_SIZE * 4;
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
        let ro = Rights::READ;

        s.kernel
            .spaces
            .get_mut(s.svc_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, rw, 0)
            .unwrap();
        s.kernel
            .spaces
            .get_mut(s.comp_space.0)
            .unwrap()
            .map_vmo(s.shared_vmo, vmo_size, ro, 0)
            .unwrap();

        for frame in 0..10 {
            let event = s.kernel.events.get_mut(s.event.0).unwrap();

            event.add_waiter(s.comp_thread, 0b1).unwrap();

            let woken = event.signal(0b1);

            assert_eq!(woken.len(), 1, "frame {frame}");
            assert_eq!(woken.as_slice()[0].thread_id, s.comp_thread);

            event.clear(0b1);
        }

        crate::invariants::assert_valid(&*s.kernel);
    }
}
