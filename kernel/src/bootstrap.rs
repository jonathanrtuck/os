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
    syscall::Kernel,
    thread::Thread,
    types::{AddressSpaceId, ObjectType, Priority, Rights, SyscallError, ThreadId, VmoId},
    vmo::{Vmo, VmoFlags},
};

pub const INIT_CODE_VA: usize = 0x0020_0000;
pub const INIT_STACK_SIZE: usize = config::PAGE_SIZE * 4;
pub const INIT_STACK_VA: usize = 0x4000_0000;

pub fn create_init(kernel: &mut Kernel, init_binary: &[u8]) -> Result<ThreadId, SyscallError> {
    if init_binary.is_empty() {
        return Err(SyscallError::InvalidArgument);
    }

    let asid = kernel.alloc_asid()?;
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let (space_idx, space_gen) = kernel
        .spaces
        .alloc(space)
        .ok_or(SyscallError::OutOfMemory)?;

    kernel.spaces.get_mut(space_idx).unwrap().id = AddressSpaceId(space_idx);

    let code_size = init_binary.len().next_multiple_of(config::PAGE_SIZE);
    let code_vmo = Vmo::new(VmoId(0), code_size, VmoFlags::NONE);
    let (code_idx, code_gen) = kernel
        .vmos
        .alloc(code_vmo)
        .ok_or(SyscallError::OutOfMemory)?;

    kernel.vmos.get_mut(code_idx).unwrap().id = VmoId(code_idx);

    let stack_vmo = Vmo::new(VmoId(0), INIT_STACK_SIZE, VmoFlags::NONE);
    let (stack_idx, _stack_gen) = kernel
        .vmos
        .alloc(stack_vmo)
        .ok_or(SyscallError::OutOfMemory)?;

    kernel.vmos.get_mut(stack_idx).unwrap().id = VmoId(stack_idx);

    let rx = Rights(Rights::READ.0 | Rights::EXECUTE.0);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    let space = kernel
        .spaces
        .get_mut(space_idx)
        .ok_or(SyscallError::InvalidArgument)?;
    let code_va = space.map_vmo(VmoId(code_idx), code_size, rx, INIT_CODE_VA)?;
    let stack_va = space.map_vmo(VmoId(stack_idx), INIT_STACK_SIZE, rw, INIT_STACK_VA)?;
    let space = kernel
        .spaces
        .get_mut(space_idx)
        .ok_or(SyscallError::InvalidArgument)?;

    space
        .handles_mut()
        .allocate(ObjectType::AddressSpace, space_idx, Rights::ALL, space_gen)?;
    space
        .handles_mut()
        .allocate(ObjectType::Vmo, code_idx, Rights::ALL, code_gen)?;

    #[cfg(target_os = "none")]
    {
        use crate::frame::{
            arch::{page_alloc, page_table},
            user_mem,
        };

        let (root, asid) = page_table::create_page_table().ok_or(SyscallError::OutOfMemory)?;
        let space = kernel
            .spaces
            .get_mut(space_idx)
            .ok_or(SyscallError::InvalidArgument)?;

        space.set_page_table(root.0, asid.0);

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
                page_table::VirtAddr(INIT_CODE_VA + offset),
                pa,
                page_table::Perms::RX,
            );
        }

        for offset in (0..INIT_STACK_SIZE).step_by(config::PAGE_SIZE) {
            let pa = page_alloc::alloc_page().ok_or(SyscallError::OutOfMemory)?;

            user_mem::zero_phys(pa.0, config::PAGE_SIZE);
            page_table::map_page(
                root,
                page_table::VirtAddr(INIT_STACK_VA + offset),
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
    let (thread_idx, _thread_gen) = kernel
        .threads
        .alloc(thread)
        .ok_or(SyscallError::OutOfMemory)?;

    kernel.threads.get_mut(thread_idx).unwrap().id = ThreadId(thread_idx);

    let core = kernel.scheduler.least_loaded_core();

    kernel
        .scheduler
        .enqueue(core, ThreadId(thread_idx), Priority::Medium);

    kernel.alive_threads += 1;

    Ok(ThreadId(thread_idx))
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    fn setup_kernel() -> Box<Kernel> {
        Box::new(Kernel::new(1))
    }

    fn fake_init_binary() -> &'static [u8] {
        &[0u8; 64]
    }

    #[test]
    fn bootstrap_creates_address_space() {
        let mut k = setup_kernel();
        let tid = create_init(&mut k, fake_init_binary()).unwrap();
        let thread = k.threads.get(tid.0).unwrap();

        assert!(thread.address_space().is_some());

        let space_id = thread.address_space().unwrap();

        assert!(k.spaces.get(space_id.0).is_some());

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_creates_code_and_stack_vmos() {
        let mut k = setup_kernel();

        create_init(&mut k, fake_init_binary()).unwrap();

        assert_eq!(k.vmos.count(), 2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_maps_code_at_expected_address() {
        let mut k = setup_kernel();
        let tid = create_init(&mut k, fake_init_binary()).unwrap();
        let thread = k.threads.get(tid.0).unwrap();

        assert_eq!(thread.entry_point(), INIT_CODE_VA);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_maps_stack() {
        let mut k = setup_kernel();
        let tid = create_init(&mut k, fake_init_binary()).unwrap();
        let thread = k.threads.get(tid.0).unwrap();

        assert_eq!(thread.stack_top(), INIT_STACK_VA + INIT_STACK_SIZE);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_installs_handles() {
        let mut k = setup_kernel();
        let tid = create_init(&mut k, fake_init_binary()).unwrap();
        let space_id = k.threads.get(tid.0).unwrap().address_space().unwrap();
        let space = k.spaces.get(space_id.0).unwrap();

        assert!(space.handles().count() >= 2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_enqueues_thread() {
        let mut k = setup_kernel();

        create_init(&mut k, fake_init_binary()).unwrap();

        assert_eq!(k.scheduler.core(0).total_ready(), 1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_rejects_empty_binary() {
        let mut k = setup_kernel();

        assert_eq!(create_init(&mut k, &[]), Err(SyscallError::InvalidArgument));

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn bootstrap_code_size_page_aligned() {
        let mut k = setup_kernel();

        create_init(&mut k, &[0u8; 100]).unwrap();

        let code_vmo = k.vmos.get(0).unwrap();

        assert!(code_vmo.size().is_multiple_of(config::PAGE_SIZE));

        crate::invariants::assert_valid(&*k);
    }
}
