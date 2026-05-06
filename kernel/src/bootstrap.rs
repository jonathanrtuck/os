//! Init bootstrap — creates the first userspace execution environment.
//!
//! The kernel embeds the init binary as raw bytes. This module creates
//! an address space, maps init's code and stack, installs bootstrap
//! handles, creates a thread, and enqueues it for scheduling. After
//! this, the scheduler picks up the init thread and context-switches
//! to EL0.

use crate::{
    address_space::AddressSpace,
    config,
    frame::state,
    thread::Thread,
    types::{AddressSpaceId, ObjectType, Priority, Rights, SyscallError, ThreadId, VmoId},
    vmo::{Vmo, VmoFlags},
};

pub const INIT_STACK_SIZE: usize = config::PAGE_SIZE * 4;

pub fn create_init(init_binary: &[u8]) -> Result<ThreadId, SyscallError> {
    if init_binary.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }

    let asid = state::alloc_asid()?;
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = state::spaces()
        .alloc_shared(space)
        .ok_or(SyscallError::OutOfMemory)?;

    {
        let mut space = state::spaces().write(space_idx).unwrap();

        space.id = AddressSpaceId(space_idx);
        #[cfg(target_os = "none")]
        space.set_aslr_seed(crate::frame::arch::entropy::random_u64());
    }

    let code_size = init_binary.len().next_multiple_of(config::PAGE_SIZE);
    let code_vmo = Vmo::new(VmoId(0), code_size, VmoFlags::NONE);
    let (code_idx, code_gen) = state::vmos()
        .alloc_shared(code_vmo)
        .ok_or(SyscallError::OutOfMemory)?;

    state::vmos().write(code_idx).unwrap().id = VmoId(code_idx);

    let stack_vmo = Vmo::new(VmoId(0), INIT_STACK_SIZE, VmoFlags::NONE);
    let (stack_idx, _stack_gen) = state::vmos()
        .alloc_shared(stack_vmo)
        .ok_or(SyscallError::OutOfMemory)?;

    state::vmos().write(stack_idx).unwrap().id = VmoId(stack_idx);

    let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let (code_va, stack_va) = {
        let mut space = state::spaces()
            .write(space_idx)
            .ok_or(SyscallError::InvalidArgument)?;
        let code_va = space.map_vmo(VmoId(code_idx), code_size, rx, 0)?;
        let stack_va = space.map_vmo(VmoId(stack_idx), INIT_STACK_SIZE, rw, 0)?;

        (code_va, stack_va)
    };

    state::vmos().write(code_idx).unwrap().inc_mapping_count();
    state::vmos().write(stack_idx).unwrap().inc_mapping_count();

    {
        let mut space = state::spaces()
            .write(space_idx)
            .ok_or(SyscallError::InvalidArgument)?;

        space.handles_mut().allocate(
            ObjectType::AddressSpace,
            space_idx,
            Rights::ALL,
            space_gen,
        )?;
        space
            .handles_mut()
            .allocate(ObjectType::Vmo, code_idx, Rights::ALL, code_gen)?;
    }

    #[cfg(target_os = "none")]
    {
        use crate::frame::{
            arch::{page_alloc, page_table},
            user_mem,
        };

        let (root, asid_val) = page_table::create_page_table().ok_or(SyscallError::OutOfMemory)?;

        {
            let mut space = state::spaces()
                .write(space_idx)
                .ok_or(SyscallError::InvalidArgument)?;

            space.set_page_table(root.0, asid_val.0);
        }

        for offset in (0..code_size).step_by(config::PAGE_SIZE) {
            let pa = page_alloc::alloc_page().ok_or(SyscallError::OutOfMemory)?;
            let chunk_end = (offset + config::PAGE_SIZE).min(init_binary.len());

            if offset < init_binary.len() {
                user_mem::write_phys(pa.0, 0, &init_binary[offset..chunk_end]);

                if chunk_end - offset < config::PAGE_SIZE {
                    user_mem::zero_phys(
                        pa.0 + (chunk_end - offset),
                        config::PAGE_SIZE - (chunk_end - offset),
                    );
                }
            } else {
                user_mem::zero_phys(pa.0, config::PAGE_SIZE);
            }

            page_table::map_page(
                root,
                page_table::VirtAddr(code_va + offset),
                pa,
                page_table::Perms::RX,
            );
        }

        for offset in (0..INIT_STACK_SIZE).step_by(config::PAGE_SIZE) {
            let pa = page_alloc::alloc_page().ok_or(SyscallError::OutOfMemory)?;

            user_mem::zero_phys(pa.0, config::PAGE_SIZE);
            page_table::map_page(
                root,
                page_table::VirtAddr(stack_va + offset),
                pa,
                page_table::Perms::RW,
            );
        }
    }

    let stack_top = stack_va + INIT_STACK_SIZE;
    let thread = Thread::new(
        ThreadId(0),
        Some(AddressSpaceId(space_idx)),
        Priority::Medium,
        code_va,
        stack_top,
        0,
    );
    let (thread_idx, _thread_gen) = state::threads()
        .alloc_shared(thread)
        .ok_or(SyscallError::OutOfMemory)?;

    {
        let mut thread = state::threads().write(thread_idx).unwrap();

        thread.id = ThreadId(thread_idx);
        thread.set_state(crate::thread::ThreadRunState::Running);
    }

    state::schedulers()
        .core(0)
        .lock()
        .set_current(Some(ThreadId(thread_idx)));
    state::inc_alive_threads();

    Ok(ThreadId(thread_idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() {
        state::init(1);
    }

    fn fake_init_binary() -> &'static [u8] {
        &[0u8; 64]
    }

    #[test]
    fn bootstrap_creates_address_space() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let thread = state::threads().read(tid.0).unwrap();

        assert!(thread.address_space().is_some());

        let space_id = thread.address_space().unwrap();

        drop(thread);

        assert!(state::spaces().read(space_id.0).is_some());

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_creates_code_and_stack_vmos() {
        setup();

        create_init(fake_init_binary()).unwrap();

        assert_eq!(state::vmos().count(), 2);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_maps_code_page_aligned() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let thread = state::threads().read(tid.0).unwrap();

        assert!(thread.entry_point() >= config::PAGE_SIZE);
        assert!(thread.entry_point().is_multiple_of(config::PAGE_SIZE));

        drop(thread);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_maps_stack() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let thread = state::threads().read(tid.0).unwrap();

        assert!(thread.stack_top() > config::PAGE_SIZE);
        assert!(thread.stack_top().is_multiple_of(config::PAGE_SIZE));

        drop(thread);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_installs_handles() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let space_id = state::threads()
            .read(tid.0)
            .unwrap()
            .address_space()
            .unwrap();
        let space = state::spaces().read(space_id.0).unwrap();

        assert!(space.handles().count() >= 2);

        drop(space);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_sets_current_thread() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();

        assert_eq!(state::schedulers().core(0).lock().current(), Some(tid));
        assert_eq!(state::schedulers().core(0).lock().total_ready(), 0);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_rejects_empty_binary() {
        setup();

        assert_eq!(create_init(&[]), Err(SyscallError::InvalidArgument));

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_code_size_page_aligned() {
        setup();
        create_init(&[0u8; 100]).unwrap();

        let code_vmo = state::vmos().read(0).unwrap();

        assert!(code_vmo.size().is_multiple_of(config::PAGE_SIZE));

        drop(code_vmo);

        crate::invariants::assert_valid();
    }

    #[test]
    fn bootstrap_increments_alive_threads() {
        setup();

        assert_eq!(state::alive_thread_count(), 0);

        create_init(fake_init_binary()).unwrap();

        assert_eq!(state::alive_thread_count(), 1);
    }

    #[test]
    fn bootstrap_handle_rights() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let space_id = state::threads()
            .read(tid.0)
            .unwrap()
            .address_space()
            .unwrap();
        let space = state::spaces().read(space_id.0).unwrap();
        let h0 = space.handles().lookup(crate::types::HandleId(0)).unwrap();

        assert_eq!(h0.object_type, ObjectType::AddressSpace);
        assert_eq!(h0.rights, Rights::ALL);

        let h1 = space.handles().lookup(crate::types::HandleId(1)).unwrap();

        assert_eq!(h1.object_type, ObjectType::Vmo);
        assert_eq!(h1.rights, Rights::ALL);
    }

    #[test]
    fn bootstrap_mapping_rights() {
        setup();

        let tid = create_init(fake_init_binary()).unwrap();
        let space_id = state::threads()
            .read(tid.0)
            .unwrap()
            .address_space()
            .unwrap();
        let space = state::spaces().read(space_id.0).unwrap();
        let mappings = space.mappings();

        assert_eq!(mappings.len(), 2);

        let code_mapping = mappings
            .iter()
            .find(|m| m.rights.contains(Rights::EXECUTE))
            .unwrap();

        assert!(code_mapping.rights.contains(Rights::READ));
        assert!(!code_mapping.rights.contains(Rights::WRITE));

        let stack_mapping = mappings
            .iter()
            .find(|m| m.rights.contains(Rights::WRITE))
            .unwrap();

        assert!(stack_mapping.rights.contains(Rights::READ));
        assert!(!stack_mapping.rights.contains(Rights::EXECUTE));
    }

    #[test]
    fn bootstrap_stack_size_is_four_pages() {
        assert_eq!(INIT_STACK_SIZE, 4 * config::PAGE_SIZE);
    }
}
