//! Syscall dispatch — maps syscall numbers to kernel object operations.
//!
//! The Kernel struct owns all object tables and the scheduler. Each syscall
//! handler extracts arguments, validates handles/rights, calls the kernel
//! object method, and returns (error_code, return_value).

use alloc::boxed::Box;

use crate::{
    address_space::AddressSpace,
    config,
    endpoint::{Endpoint, PendingCall, ReplyCapId},
    event::Event,
    frame::user_mem,
    handle::Handle,
    irq::IrqTable,
    table::ObjectTable,
    thread::{Scheduler, Thread},
    types::{
        AddressSpaceId, EndpointId, EventId, HandleId, ObjectType, Priority, Rights, SyscallError,
        ThreadId, VmoId,
    },
    vmo::{Vmo, VmoFlags},
};

/// Syscall numbers (30 total).
pub mod num {
    pub const VMO_CREATE: u64 = 0;
    pub const VMO_MAP: u64 = 1;
    pub const VMO_MAP_INTO: u64 = 2;
    pub const VMO_UNMAP: u64 = 3;
    pub const VMO_SNAPSHOT: u64 = 4;
    pub const VMO_SEAL: u64 = 5;
    pub const VMO_RESIZE: u64 = 6;
    pub const VMO_SET_PAGER: u64 = 7;
    pub const ENDPOINT_CREATE: u64 = 8;
    pub const CALL: u64 = 9;
    pub const RECV: u64 = 10;
    pub const REPLY: u64 = 11;
    pub const EVENT_CREATE: u64 = 12;
    pub const EVENT_SIGNAL: u64 = 13;
    pub const EVENT_WAIT: u64 = 14;
    pub const EVENT_CLEAR: u64 = 15;
    pub const THREAD_CREATE: u64 = 16;
    pub const THREAD_CREATE_IN: u64 = 17;
    pub const THREAD_EXIT: u64 = 18;
    pub const THREAD_SET_PRIORITY: u64 = 19;
    pub const THREAD_SET_AFFINITY: u64 = 20;
    pub const SPACE_CREATE: u64 = 21;
    pub const SPACE_DESTROY: u64 = 22;
    pub const HANDLE_DUP: u64 = 23;
    pub const HANDLE_CLOSE: u64 = 24;
    pub const HANDLE_INFO: u64 = 25;
    pub const CLOCK_READ: u64 = 26;
    pub const SYSTEM_INFO: u64 = 27;
    pub const EVENT_BIND_IRQ: u64 = 28;
    pub const ENDPOINT_BIND_EVENT: u64 = 29;
}

struct StagedHandles {
    handles: [Option<Handle>; config::MAX_IPC_HANDLES],
    count: u8,
}

impl StagedHandles {
    fn new() -> Self {
        StagedHandles {
            handles: [const { None }; config::MAX_IPC_HANDLES],
            count: 0,
        }
    }
}

/// Central kernel state — all object tables and the scheduler.
pub struct Kernel {
    pub vmos: ObjectTable<Vmo, { config::MAX_VMOS }>,
    pub events: ObjectTable<Event, { config::MAX_EVENTS }>,
    pub endpoints: ObjectTable<Endpoint, { config::MAX_ENDPOINTS }>,
    pub threads: ObjectTable<Thread, { config::MAX_THREADS }>,
    pub spaces: ObjectTable<AddressSpace, { config::MAX_ADDRESS_SPACES }>,
    pub irqs: Box<IrqTable>,
    pub scheduler: Scheduler,
    /// Core ID of the current syscall dispatch. Set at dispatch entry from
    /// percpu (bare metal) or 0 (host tests). Used by all scheduler calls
    /// instead of hardcoding core 0.
    core_id: usize,
    /// Number of threads that have not yet exited. When this reaches zero
    /// on bare metal, the kernel issues PSCI SYSTEM_OFF.
    pub(crate) alive_threads: u32,
}

impl Kernel {
    pub fn new(num_cores: usize) -> Self {
        Kernel {
            vmos: ObjectTable::new(),
            events: ObjectTable::new(),
            endpoints: ObjectTable::new(),
            threads: ObjectTable::new(),
            spaces: ObjectTable::new(),
            irqs: Box::new(IrqTable::new()),
            scheduler: Scheduler::new(num_cores),
            core_id: 0,
            alive_threads: 0,
        }
    }

    #[cfg(any(target_os = "none", test))]
    pub fn alloc_asid(&self) -> Result<u8, SyscallError> {
        crate::frame::arch::page_table::alloc_asid()
            .map(|asid| asid.0)
            .ok_or(SyscallError::OutOfMemory)
    }

    #[cfg(not(any(target_os = "none", test)))]
    pub fn alloc_asid(&self) -> Result<u8, SyscallError> {
        Err(SyscallError::OutOfMemory)
    }

    #[cfg(any(target_os = "none", test))]
    fn init_thread_registers(thread: &mut Thread, entry: usize, stack_top: usize, arg: usize) {
        let rs = thread.init_register_state();

        rs.pc = entry as u64;
        rs.sp = stack_top as u64;
        rs.gprs[0] = arg as u64;
        // SPSR_EL1 = 0: EL0t, all interrupts unmasked, AArch64
        rs.pstate = 0;

        // x30 (LR) is restored by context_switch and then `ret`'d to.
        // Point it at the trampoline that enters userspace with this
        // thread's full RegisterState.
        #[cfg(target_os = "none")]
        {
            rs.gprs[30] = crate::frame::arch::context::new_thread_trampoline() as u64;
        }
    }

    #[cfg(not(any(target_os = "none", test)))]
    fn init_thread_registers(_thread: &mut Thread, _entry: usize, _sp: usize, _arg: usize) {}

    #[cfg(target_os = "none")]
    fn free_kernel_stack(&self, thread_idx: u32) {
        let base = self.threads.get(thread_idx).unwrap().kernel_stack_base();

        if base != 0 {
            let base_pa = crate::frame::arch::platform::virt_to_phys(base);

            for i in 0..config::KERNEL_STACK_PAGES {
                crate::frame::arch::page_alloc::release(crate::frame::arch::page_alloc::PhysAddr(
                    base_pa + i * config::PAGE_SIZE,
                ));
            }
        }
    }

    fn link_thread_to_space(&mut self, thread_idx: u32, space_id: AddressSpaceId) {
        let old_head = self.spaces.get(space_id.0).and_then(|s| s.thread_head());

        if let Some(t) = self.threads.get_mut(thread_idx) {
            t.set_space_next(old_head);
            t.set_space_prev(None);
        }

        if let Some(old) = old_head
            && let Some(t) = self.threads.get_mut(old)
        {
            t.set_space_prev(Some(thread_idx));
        }

        if let Some(s) = self.spaces.get_mut(space_id.0) {
            s.set_thread_head(Some(thread_idx));
        }
    }

    fn unlink_thread_from_space(&mut self, thread_idx: u32, space_id: AddressSpaceId) {
        let prev = self.threads.get(thread_idx).and_then(|t| t.space_prev());
        let next = self.threads.get(thread_idx).and_then(|t| t.space_next());

        if let Some(p) = prev {
            if let Some(t) = self.threads.get_mut(p) {
                t.set_space_next(next);
            }
        } else if let Some(s) = self.spaces.get_mut(space_id.0) {
            s.set_thread_head(next);
        }

        if let Some(n) = next
            && let Some(t) = self.threads.get_mut(n)
        {
            t.set_space_prev(prev);
        }

        if let Some(t) = self.threads.get_mut(thread_idx) {
            t.set_space_next(None);
            t.set_space_prev(None);
        }
    }

    pub fn thread_space_id(&self, thread: ThreadId) -> Result<AddressSpaceId, SyscallError> {
        self.threads
            .get(thread.0)
            .ok_or(SyscallError::InvalidArgument)?
            .address_space()
            .ok_or(SyscallError::InvalidArgument)
    }

    fn lookup_handle(
        &self,
        space_id: AddressSpaceId,
        handle_id: HandleId,
    ) -> Result<Handle, SyscallError> {
        let space = self
            .spaces
            .get(space_id.0)
            .ok_or(SyscallError::InvalidHandle)?;
        let handle = space.handles().lookup(handle_id)?.clone();
        let current_gen = match handle.object_type {
            ObjectType::Vmo => self.vmos.generation(handle.object_id),
            ObjectType::Endpoint => self.endpoints.generation(handle.object_id),
            ObjectType::Event => self.events.generation(handle.object_id),
            ObjectType::Thread => self.threads.generation(handle.object_id),
            ObjectType::AddressSpace => self.spaces.generation(handle.object_id),
        };

        if handle.generation != current_gen {
            return Err(SyscallError::GenerationMismatch);
        }

        Ok(handle)
    }

    fn remove_handles_atomic(
        &mut self,
        space_id: AddressSpaceId,
        handles_ptr: usize,
        count: usize,
    ) -> Result<StagedHandles, SyscallError> {
        let mut staged = StagedHandles::new();

        if count == 0 {
            return Ok(staged);
        }

        let mut ids = [0u32; config::MAX_IPC_HANDLES];

        user_mem::read_user_u32s(handles_ptr, count, &mut ids)?;

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidHandle)?;
        let ht = space.handles_mut();

        for (i, &id) in ids[..count].iter().enumerate() {
            match ht.remove(HandleId(id)) {
                Ok(h) => {
                    staged.handles[i] = Some(h);
                    staged.count = (i + 1) as u8;
                }
                Err(e) => {
                    for slot in &mut staged.handles[..i] {
                        if let Some(h) = slot.take() {
                            let _ = ht.install(h);
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(staged)
    }

    fn reinstall_handles(&mut self, space_id: AddressSpaceId, mut staged: StagedHandles) {
        if let Some(space) = self.spaces.get_mut(space_id.0) {
            let ht = space.handles_mut();

            for i in 0..staged.count as usize {
                if let Some(h) = staged.handles[i].take() {
                    let _ = ht.install(h);
                }
            }
        }
    }

    fn install_handles(
        &mut self,
        space_id: AddressSpaceId,
        staged: &mut StagedHandles,
        out_ptr: usize,
        out_cap: usize,
    ) -> Result<usize, SyscallError> {
        let count = staged.count as usize;

        if count > out_cap {
            return Err(SyscallError::BufferFull);
        }

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidHandle)?;
        let ht = space.handles_mut();
        let mut new_ids = [0u32; config::MAX_IPC_HANDLES];

        for (slot, out_id) in staged.handles[..count].iter_mut().zip(new_ids.iter_mut()) {
            let h = slot.take().unwrap();

            *out_id = ht.install(h)?.0;
        }

        user_mem::write_user_u32s(out_ptr, &new_ids[..count])?;

        Ok(count)
    }

    /// Main dispatch — routes syscall number to handler.
    #[inline(never)]
    pub fn dispatch(
        &mut self,
        current: ThreadId,
        core_id: usize,
        syscall_num: u64,
        args: &[u64; 6],
    ) -> (u64, u64) {
        self.core_id = core_id;

        let result = match syscall_num {
            num::VMO_CREATE => self.sys_vmo_create(current, args),
            num::VMO_MAP => self.sys_vmo_map(current, args),
            num::VMO_MAP_INTO => self.sys_vmo_map_into(current, args),
            num::VMO_UNMAP => self.sys_vmo_unmap(current, args),
            num::VMO_SNAPSHOT => self.sys_vmo_snapshot(current, args),
            num::VMO_SEAL => self.sys_vmo_seal(current, args),
            num::VMO_RESIZE => self.sys_vmo_resize(current, args),
            num::VMO_SET_PAGER => self.sys_vmo_set_pager(current, args),
            num::ENDPOINT_CREATE => self.sys_endpoint_create(current, args),
            num::CALL => self.sys_call(current, args),
            num::RECV => self.sys_recv(current, args),
            num::REPLY => self.sys_reply(current, args),
            num::EVENT_CREATE => self.sys_event_create(current, args),
            num::EVENT_SIGNAL => self.sys_event_signal(current, args),
            num::EVENT_WAIT => self.sys_event_wait(current, args),
            num::EVENT_CLEAR => self.sys_event_clear(current, args),
            num::THREAD_CREATE => self.sys_thread_create(current, args),
            num::THREAD_CREATE_IN => self.sys_thread_create_in(current, args),
            num::THREAD_EXIT => self.sys_thread_exit(current, args),
            num::THREAD_SET_PRIORITY => self.sys_thread_set_priority(current, args),
            num::THREAD_SET_AFFINITY => self.sys_thread_set_affinity(current, args),
            num::SPACE_CREATE => self.sys_space_create(current, args),
            num::SPACE_DESTROY => self.sys_space_destroy(current, args),
            num::HANDLE_DUP => self.sys_handle_dup(current, args),
            num::HANDLE_CLOSE => self.sys_handle_close(current, args),
            num::HANDLE_INFO => self.sys_handle_info(current, args),
            num::CLOCK_READ => self.sys_clock_read(args),
            num::SYSTEM_INFO => self.sys_system_info(args),
            num::EVENT_BIND_IRQ => self.sys_event_bind_irq(current, args),
            num::ENDPOINT_BIND_EVENT => self.sys_endpoint_bind_event(current, args),
            _ => Err(SyscallError::InvalidArgument),
        };

        match result {
            Ok(value) => (0, value),
            Err(e) => (e as u64, 0),
        }
    }

    // -- VMO syscalls --

    fn sys_vmo_create(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let size = args[0] as usize;
        let flags = args[1] as u32;

        if size == 0 || size > config::MAX_PHYS_MEM {
            return Err(SyscallError::InvalidArgument);
        }

        let known_flags = VmoFlags::HINT_CONTIGUOUS.0;

        if flags & !known_flags != 0 {
            return Err(SyscallError::InvalidArgument);
        }

        let space_id = self.thread_space_id(current)?;
        let vmo = Vmo::new(VmoId(0), size, VmoFlags(flags));
        let (idx, generation) = self.vmos.alloc(vmo).ok_or(SyscallError::OutOfMemory)?;

        self.vmos.get_mut(idx).unwrap().id = VmoId(idx);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Vmo, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.vmos.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_vmo_map(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let addr_hint = args[1] as usize;
        let perms = Rights(args[2] as u32);
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::MAP) {
            return Err(SyscallError::InsufficientRights);
        }
        if perms.contains(Rights::WRITE) && !handle.rights.contains(Rights::WRITE) {
            return Err(SyscallError::InsufficientRights);
        }

        let vmo_size = self
            .vmos
            .get(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .size();
        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let va = space.map_vmo(VmoId(handle.object_id), vmo_size, perms, addr_hint)?;

        Ok(va as u64)
    }

    fn sys_vmo_unmap(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let addr = args[0] as usize;
        let space_id = self.thread_space_id(current)?;
        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        space.unmap(addr)?;

        Ok(0)
    }

    fn sys_vmo_snapshot(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }

        let parent = self
            .vmos
            .get(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let snap = parent.snapshot(VmoId(0));
        let (idx, generation) = self.vmos.alloc(snap).ok_or(SyscallError::OutOfMemory)?;

        self.vmos.get_mut(idx).unwrap().id = VmoId(idx);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Vmo, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.vmos.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_vmo_seal(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::WRITE) {
            return Err(SyscallError::InsufficientRights);
        }

        self.vmos
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .seal()?;

        Ok(0)
    }

    fn sys_vmo_resize(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let new_size = args[1] as usize;

        if new_size > config::MAX_PHYS_MEM {
            return Err(SyscallError::InvalidArgument);
        }

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::WRITE) {
            return Err(SyscallError::InsufficientRights);
        }

        let vmo_id = handle.object_id;
        let aligned_new = new_size.next_multiple_of(config::PAGE_SIZE);

        for (_, space) in self.spaces.iter_allocated_mut() {
            for m in space.mappings() {
                if m.vmo_id.0 == vmo_id && m.size > aligned_new {
                    return Err(SyscallError::InvalidArgument);
                }
            }
        }

        self.vmos
            .get_mut(vmo_id)
            .ok_or(SyscallError::InvalidHandle)?
            .resize(new_size, |_pa| {
                #[cfg(target_os = "none")]
                crate::frame::arch::page_alloc::release(crate::frame::arch::page_alloc::PhysAddr(
                    _pa,
                ));
            })?;

        Ok(0)
    }

    // -- Endpoint syscalls --

    fn sys_endpoint_create(
        &mut self,
        current: ThreadId,
        _args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let space_id = self.thread_space_id(current)?;
        let ep = Endpoint::new(EndpointId(0));
        let (idx, generation) = self.endpoints.alloc(ep).ok_or(SyscallError::OutOfMemory)?;

        self.endpoints.get_mut(idx).unwrap().id = EndpointId(idx);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Endpoint, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.endpoints.dealloc(idx);

                Err(e)
            }
        }
    }

    // -- Event syscalls --

    fn sys_event_create(
        &mut self,
        current: ThreadId,
        _args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let space_id = self.thread_space_id(current)?;
        let event = Event::new(EventId(0));
        let (idx, generation) = self.events.alloc(event).ok_or(SyscallError::OutOfMemory)?;

        self.events.get_mut(idx).unwrap().id = EventId(idx);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Event, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.events.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_event_signal(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let bits = args[1];
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Event {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::SIGNAL) {
            return Err(SyscallError::InsufficientRights);
        }

        let woken = self
            .events
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .signal(bits);

        for info in woken.as_slice() {
            crate::sched::wake(self, info.thread_id, self.core_id);
        }

        Ok(0)
    }

    fn sys_event_clear(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let bits = args[1];
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Event {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::SIGNAL) {
            return Err(SyscallError::InsufficientRights);
        }

        let event = self
            .events
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        event.clear(bits);

        let (intids, count) = self
            .irqs
            .intids_for_event_bits(EventId(handle.object_id), bits);

        for &intid in &intids[..count] {
            if self.irqs.ack(intid).is_ok() {
                #[cfg(target_os = "none")]
                crate::frame::arch::gic::unmask_spi(intid);
            }
        }

        Ok(0)
    }

    // -- Space syscalls --

    fn sys_space_create(
        &mut self,
        current: ThreadId,
        _args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let caller_space_id = self.thread_space_id(current)?;
        let asid = self.alloc_asid()?;
        let space = Box::new(AddressSpace::new(AddressSpaceId(0), asid, 0));
        let (idx, generation) = self
            .spaces
            .alloc_boxed(space)
            .ok_or(SyscallError::OutOfMemory)?;

        self.spaces.get_mut(idx).unwrap().id = AddressSpaceId(idx);

        let caller_space = self
            .spaces
            .get_mut(caller_space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match caller_space.handles_mut().allocate(
            ObjectType::AddressSpace,
            idx,
            Rights::ALL,
            generation,
        ) {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.spaces.dealloc(idx);

                Err(e)
            }
        }
    }

    // -- Handle syscalls --

    fn sys_handle_dup(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let new_rights = Rights(args[1] as u32);
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if !handle.rights.contains(Rights::DUP) {
            return Err(SyscallError::InsufficientRights);
        }

        let obj_type = handle.object_type;
        let obj_id = handle.object_id;
        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let new_id = space.handles_mut().duplicate(handle_id, new_rights)?;

        self.add_object_ref(obj_type, obj_id);

        Ok(new_id.0 as u64)
    }

    fn add_object_ref(&mut self, object_type: ObjectType, object_id: u32) {
        match object_type {
            ObjectType::Vmo => {
                if let Some(vmo) = self.vmos.get_mut(object_id) {
                    vmo.add_ref();
                }
            }
            ObjectType::Endpoint => {
                if let Some(ep) = self.endpoints.get_mut(object_id) {
                    ep.add_ref();
                }
            }
            ObjectType::Event => {
                if let Some(evt) = self.events.get_mut(object_id) {
                    evt.add_ref();
                }
            }
            ObjectType::Thread | ObjectType::AddressSpace => {}
        }
    }

    fn sys_handle_close(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let space_id = self.thread_space_id(current)?;
        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let handle = space.handles_mut().close(handle_id)?;

        self.release_object_ref(handle.object_type, handle.object_id);

        Ok(0)
    }

    fn release_object_ref(&mut self, object_type: ObjectType, object_id: u32) {
        match object_type {
            ObjectType::Vmo => {
                if let Some(vmo) = self.vmos.get_mut(object_id)
                    && vmo.release_ref()
                {
                    self.vmos.dealloc(object_id);
                }
            }
            ObjectType::Endpoint => {
                if let Some(ep) = self.endpoints.get_mut(object_id)
                    && ep.release_ref()
                {
                    self.close_endpoint_peer(object_id);

                    if let Some(ep) = self.endpoints.get_mut(object_id)
                        && let Some(evt_id) = ep.bound_event()
                        && let Some(evt) = self.events.get_mut(evt_id.0)
                    {
                        evt.unbind_endpoint();
                    }

                    self.endpoints.dealloc(object_id);
                }
            }
            ObjectType::Event => {
                if let Some(evt) = self.events.get_mut(object_id)
                    && evt.release_ref()
                {
                    self.destroy_event(object_id);
                }
            }
            ObjectType::Thread | ObjectType::AddressSpace => {}
        }
    }

    fn close_endpoint_peer(&mut self, ep_id: u32) {
        let Some(ep) = self.endpoints.get_mut(ep_id) else {
            return;
        };

        let mut close_result = ep.close_peer();

        for canceled in close_result.canceled_callers_mut() {
            if let Some(caller) = canceled.take() {
                if caller.handle_count > 0 {
                    let caller_space = self
                        .threads
                        .get(caller.thread_id.0)
                        .and_then(|t| t.address_space());

                    if let Some(sid) = caller_space {
                        self.reinstall_handles(
                            sid,
                            StagedHandles {
                                handles: caller.handles,
                                count: caller.handle_count,
                            },
                        );
                    }
                }

                if let Some(t) = self.threads.get_mut(caller.thread_id.0) {
                    t.set_wakeup_error(SyscallError::PeerClosed);

                    #[cfg(any(target_os = "none", test))]
                    if let Some(rs) = t.register_state_mut() {
                        rs.gprs[0] = SyscallError::PeerClosed as u64;
                    }
                }

                crate::sched::wake(self, caller.thread_id, self.core_id);
            }
        }

        for &tid in close_result.reply_callers() {
            if let Some(t) = self.threads.get_mut(tid.0) {
                t.set_wakeup_error(SyscallError::PeerClosed);

                #[cfg(any(target_os = "none", test))]
                if let Some(rs) = t.register_state_mut() {
                    rs.gprs[0] = SyscallError::PeerClosed as u64;
                }
            }

            crate::sched::wake(self, tid, self.core_id);
        }

        for &tid in close_result.recv_waiters() {
            if let Some(t) = self.threads.get_mut(tid.0) {
                t.set_wakeup_error(SyscallError::PeerClosed);
            }

            crate::sched::wake(self, tid, self.core_id);
        }
    }

    fn destroy_event(&mut self, event_id: u32) {
        if let Some(evt) = self.events.get(event_id)
            && let Some(ep_id) = evt.bound_endpoint()
            && let Some(ep) = self.endpoints.get_mut(ep_id.0)
        {
            ep.unbind_event();
        }

        for intid in 0..config::MAX_IRQS {
            if self
                .irqs
                .binding_at(intid)
                .is_some_and(|b| b.event_id.0 == event_id)
            {
                let _ = self.irqs.unbind(intid as u32);
            }
        }

        self.events.dealloc(event_id);
    }

    fn sys_handle_info(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        Ok(((handle.object_type as u64) << 32) | (handle.rights.0 as u64))
    }

    fn sys_clock_read(&self, _args: &[u64; 6]) -> Result<u64, SyscallError> {
        #[cfg(any(target_os = "none", test))]
        {
            let ticks = crate::frame::arch::timer::now();
            let freq = crate::frame::arch::timer::frequency();

            if freq == 0 {
                return Ok(0);
            }

            let secs = ticks / freq;
            let remainder = ticks % freq;

            Ok(secs * 1_000_000_000 + remainder * 1_000_000_000 / freq)
        }

        #[cfg(not(any(target_os = "none", test)))]
        Ok(0)
    }

    fn sys_system_info(&self, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let what = args[0];

        match what {
            0 => Ok(crate::config::PAGE_SIZE as u64),
            1 => Ok(crate::endpoint::MSG_SIZE as u64),
            2 => Ok(self.scheduler.num_cores() as u64),
            _ => Err(SyscallError::InvalidArgument),
        }
    }

    // -- Event blocking --

    fn sys_event_wait(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let space_id = self.thread_space_id(current)?;

        if args[0] as u32 > config::MAX_HANDLES as u32 {
            return self.event_wait_buffer(current, space_id, args);
        }

        self.event_wait_register(current, space_id, args)
    }

    fn event_wait_register(
        &mut self,
        current: ThreadId,
        space_id: AddressSpaceId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let mut wait_items: [(u32, u32, u64); 3] = [(0, 0, 0); 3];
        let mut count = 0usize;

        for i in 0..3 {
            let hid_raw = args[i * 2] as u32;
            let mask = args[i * 2 + 1];

            if mask == 0 {
                continue;
            }

            let handle = self.lookup_handle(space_id, HandleId(hid_raw))?;

            if handle.object_type != ObjectType::Event {
                return Err(SyscallError::WrongHandleType);
            }
            if !handle.rights.contains(Rights::WAIT) {
                return Err(SyscallError::InsufficientRights);
            }

            wait_items[count] = (hid_raw, handle.object_id, mask);
            count += 1;
        }

        if count == 0 {
            return Err(SyscallError::InvalidArgument);
        }

        self.event_wait_common(current, &wait_items[..count])
    }

    fn event_wait_buffer(
        &mut self,
        current: ThreadId,
        space_id: AddressSpaceId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let user_ptr = args[0] as usize;
        let count = args[1] as usize;

        if count == 0 || count > config::MAX_MULTI_WAIT {
            return Err(SyscallError::InvalidArgument);
        }

        let mut raw = [0u32; config::MAX_MULTI_WAIT * 3];

        user_mem::read_user_u32s(user_ptr, count * 3, &mut raw)?;

        let mut wait_items = [(0u32, 0u32, 0u64); config::MAX_MULTI_WAIT];
        #[allow(unused_mut)]
        let mut valid = 0;

        for i in 0..count {
            let hid_raw = raw[i * 3];
            let mask_lo = raw[i * 3 + 1] as u64;
            let mask_hi = raw[i * 3 + 2] as u64;
            let mask = mask_lo | (mask_hi << 32);

            if mask == 0 {
                continue;
            }

            let handle = self.lookup_handle(space_id, HandleId(hid_raw))?;

            if handle.object_type != ObjectType::Event {
                return Err(SyscallError::WrongHandleType);
            }
            if !handle.rights.contains(Rights::WAIT) {
                return Err(SyscallError::InsufficientRights);
            }

            wait_items[valid] = (hid_raw, handle.object_id, mask);
            valid += 1;
        }

        if valid == 0 {
            return Err(SyscallError::InvalidArgument);
        }

        self.event_wait_common(current, &wait_items[..valid])
    }

    fn event_wait_common(
        &mut self,
        current: ThreadId,
        wait_items: &[(u32, u32, u64)],
    ) -> Result<u64, SyscallError> {
        // First pass: check for immediate satisfaction.
        for &(hid, obj_id, mask) in wait_items {
            let event = self
                .events
                .get_mut(obj_id)
                .ok_or(SyscallError::InvalidHandle)?;

            if event.check(mask).is_some() {
                return Ok(hid as u64);
            }
        }

        // No event ready — register waiters on ALL events (not just first 3).
        let mut obj_ids = [0u32; config::MAX_MULTI_WAIT];
        let use_count = wait_items.len().min(config::MAX_MULTI_WAIT);

        for (i, &(_, obj_id, mask)) in wait_items[..use_count].iter().enumerate() {
            let event = self
                .events
                .get_mut(obj_id)
                .ok_or(SyscallError::InvalidHandle)?;

            if let Err(e) = event.add_waiter(current, mask) {
                for &prev_id in &obj_ids[..i] {
                    if let Some(prev_event) = self.events.get_mut(prev_id) {
                        prev_event.remove_waiter(current);
                    }
                }

                return Err(e);
            }

            obj_ids[i] = obj_id;
        }

        self.threads
            .get_mut(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .set_wait_events(&obj_ids[..use_count]);

        crate::sched::block_current(self, current, self.core_id);

        // Post-wakeup: find which event fired and clean up other waiters.
        let (wait_evts, wait_n) = self
            .threads
            .get_mut(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .take_wait_events();

        for i in 0..wait_n as usize {
            let obj_id = wait_evts[i];
            // The event may have been destroyed while we were blocked.
            let Some(&(hid, _, mask)) = wait_items.iter().find(|&&(_, oid, _)| oid == obj_id)
            else {
                continue;
            };
            let Some(event) = self.events.get_mut(obj_id) else {
                continue;
            };

            if event.check(mask).is_some() {
                // Remove ourselves from all other events' waiter lists.
                for &evt_id in &wait_evts[..wait_n as usize] {
                    if evt_id != obj_id
                        && let Some(e) = self.events.get_mut(evt_id)
                    {
                        e.remove_waiter(current);
                    }
                }

                return Ok(hid as u64);
            }
        }

        // Spurious wakeup or all events destroyed — clean up any remaining.
        for &evt_id in &wait_evts[..wait_n as usize] {
            if let Some(e) = self.events.get_mut(evt_id) {
                e.remove_waiter(current);
            }
        }

        Ok(0)
    }

    // -- IPC blocking --

    fn sys_call(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let msg_ptr = args[1] as usize;
        let msg_len = args[2] as usize;
        let handles_ptr = args[3] as usize;
        let handles_count = args[4] as usize;

        if handles_count > config::MAX_IPC_HANDLES {
            return Err(SyscallError::InvalidArgument);
        }

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let ep_obj_id = handle.object_id;

        let ep = self
            .endpoints
            .get(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if ep.is_peer_closed() {
            return Err(SyscallError::PeerClosed);
        }
        if ep.is_full() {
            return Err(SyscallError::BufferFull);
        }

        let message = user_mem::read_user_message(msg_ptr, msg_len)?;
        let staged = self.remove_handles_atomic(space_id, handles_ptr, handles_count)?;
        let priority = self
            .threads
            .get(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .effective_priority();
        let call = PendingCall {
            caller: current,
            priority,
            message,
            handles: staged.handles,
            handle_count: staged.count,
            badge: 0,
            reply_buf: msg_ptr,
        };
        let ep = self
            .endpoints
            .get_mut(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let signal_info = ep
            .enqueue_call(call)
            .expect("enqueue_call failed after pre-check passed");
        let active_server = ep.active_server();
        let recv_waiters = ep.drain_recv_waiters();

        if let Some(server_tid) = active_server {
            let caller_pri = self
                .threads
                .get(current.0)
                .map(|t| t.effective_priority())
                .unwrap_or(Priority::Idle);

            if let Some(server) = self.threads.get_mut(server_tid.0) {
                server.boost_priority(caller_pri);
            }
        }

        if let Some((event_id, bits)) = signal_info
            && let Some(event) = self.events.get_mut(event_id.0)
        {
            let woken = event.signal(bits);

            for info in woken.as_slice() {
                crate::sched::wake(self, info.thread_id, self.core_id);
            }
        }

        for waiter in recv_waiters.as_slice() {
            crate::sched::wake(self, *waiter, self.core_id);
        }

        crate::sched::block_current(self, current, self.core_id);

        if let Some(err) = self
            .threads
            .get_mut(current.0)
            .and_then(|t| t.take_wakeup_error())
        {
            return Err(err);
        }

        Ok(0)
    }

    fn sys_recv(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let out_buf = args[1] as usize;
        let out_cap = args[2] as usize;
        let handles_out = args[3] as usize;
        let handles_cap = args[4] as usize;
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let obj_id = handle.object_id;

        if let Some(result) = self.try_dequeue_and_deliver(
            obj_id,
            current,
            space_id,
            out_buf,
            out_cap,
            handles_out,
            handles_cap,
        ) {
            return result;
        }

        let ep = self
            .endpoints
            .get(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if ep.is_peer_closed() {
            return Err(SyscallError::PeerClosed);
        }

        let ep = self
            .endpoints
            .get_mut(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        ep.add_recv_waiter(current)?;
        crate::sched::block_current(self, current, self.core_id);

        if let Some(err) = self
            .threads
            .get_mut(current.0)
            .and_then(|t| t.take_wakeup_error())
        {
            return Err(err);
        }

        if let Some(result) = self.try_dequeue_and_deliver(
            obj_id,
            current,
            space_id,
            out_buf,
            out_cap,
            handles_out,
            handles_cap,
        ) {
            return result;
        }

        if self
            .endpoints
            .get(obj_id)
            .is_some_and(|ep| !ep.is_peer_closed())
        {
            return Err(SyscallError::TimedOut);
        }

        Err(SyscallError::PeerClosed)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_dequeue_and_deliver(
        &mut self,
        ep_obj_id: u32,
        server: ThreadId,
        space_id: AddressSpaceId,
        out_buf: usize,
        out_cap: usize,
        handles_out: usize,
        handles_cap: usize,
    ) -> Option<Result<u64, SyscallError>> {
        let ep = self.endpoints.get_mut(ep_obj_id)?;
        let (call, reply_cap) = ep.dequeue_call()?;

        ep.set_active_server(Some(server));

        if let Some((eid, bits)) = Self::check_clear_readable(ep)
            && let Some(e) = self.events.get_mut(eid.0)
        {
            e.clear(bits);
        }

        Some(self.recv_deliver(
            space_id,
            call,
            reply_cap,
            out_buf,
            out_cap,
            handles_out,
            handles_cap,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn recv_deliver(
        &mut self,
        space_id: AddressSpaceId,
        mut call: PendingCall,
        reply_cap: ReplyCapId,
        out_buf: usize,
        out_cap: usize,
        handles_out: usize,
        handles_cap: usize,
    ) -> Result<u64, SyscallError> {
        let msg_bytes = call.message.as_bytes();

        if msg_bytes.len() > out_cap {
            return Err(SyscallError::BufferFull);
        }

        user_mem::write_user_bytes(out_buf, msg_bytes)?;

        let msg_len = msg_bytes.len() as u64;
        let mut staged = StagedHandles {
            handles: core::mem::replace(
                &mut call.handles,
                [const { None }; config::MAX_IPC_HANDLES],
            ),
            count: call.handle_count,
        };
        let h_count = if staged.count > 0 {
            self.install_handles(space_id, &mut staged, handles_out, handles_cap)? as u64
        } else {
            0
        };

        Ok((reply_cap.0 as u64) << 32 | (h_count << 16) | msg_len)
    }

    fn sys_reply(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let reply_cap_id = ReplyCapId(args[1] as u32);
        let msg_ptr = args[2] as usize;
        let msg_len = args[3] as usize;
        let handles_ptr = args[4] as usize;
        let handles_count = args[5] as usize;

        if handles_count > config::MAX_IPC_HANDLES {
            return Err(SyscallError::InvalidArgument);
        }

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let reply_msg = user_mem::read_user_message(msg_ptr, msg_len)?;
        let ep = self
            .endpoints
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let (caller_id, caller_reply_buf) = ep.consume_reply(reply_cap_id)?;
        let next_highest = ep.highest_caller_priority();

        if let Some(pri) = next_highest {
            if let Some(server) = self.threads.get_mut(current.0) {
                server.boost_priority(pri);
            }
        } else if let Some(server) = self.threads.get_mut(current.0) {
            server.release_boost();
        }

        let caller = self
            .threads
            .get(caller_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        if caller.state() != crate::thread::ThreadRunState::Blocked {
            return Err(SyscallError::InvalidArgument);
        }

        // Pre-check: verify caller has room for transferred handles before
        // removing them from the server. Prevents handle leak on partial failure.
        if handles_count > 0 {
            let caller_space_id = caller
                .address_space()
                .ok_or(SyscallError::InvalidArgument)?;
            let caller_space = self
                .spaces
                .get(caller_space_id.0)
                .ok_or(SyscallError::InvalidHandle)?;

            if caller_space.handles().free_slot_count() < handles_count {
                return Err(SyscallError::BufferFull);
            }
        }

        let staged = self.remove_handles_atomic(space_id, handles_ptr, handles_count)?;

        if let Err(e) = user_mem::write_user_bytes(caller_reply_buf, reply_msg.as_bytes()) {
            self.reinstall_handles(space_id, staged);

            return Err(e);
        }

        if staged.count > 0 {
            let caller_space_id = self
                .threads
                .get(caller_id.0)
                .ok_or(SyscallError::InvalidArgument)?
                .address_space()
                .ok_or(SyscallError::InvalidArgument)?;
            let mut staged = staged;
            let caller_ht = self
                .spaces
                .get_mut(caller_space_id.0)
                .ok_or(SyscallError::InvalidHandle)?
                .handles_mut();

            for i in 0..staged.count as usize {
                if let Some(h) = staged.handles[i].take() {
                    let result = caller_ht.install(h);

                    debug_assert!(result.is_ok(), "handle install failed despite pre-check");
                }
            }
        }

        crate::sched::wake(self, caller_id, self.core_id);

        Ok(reply_msg.len() as u64)
    }

    fn check_clear_readable(ep: &Endpoint) -> Option<(EventId, u64)> {
        if ep.has_pending_calls() {
            return None;
        }

        ep.bound_event()
            .map(|eid| (eid, Endpoint::ENDPOINT_READABLE_BIT))
    }

    fn sys_endpoint_bind_event(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let ep_handle_id = HandleId(args[0] as u32);
        let event_handle_id = HandleId(args[1] as u32);
        let space_id = self.thread_space_id(current)?;
        let ep_handle = self.lookup_handle(space_id, ep_handle_id)?;

        if ep_handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }
        if !ep_handle.rights.contains(Rights::WRITE) {
            return Err(SyscallError::InsufficientRights);
        }
        let event_handle = self.lookup_handle(space_id, event_handle_id)?;
        if event_handle.object_type != ObjectType::Event {
            return Err(SyscallError::WrongHandleType);
        }
        if !event_handle.rights.contains(Rights::SIGNAL) {
            return Err(SyscallError::InsufficientRights);
        }

        let event_obj_id = EventId(event_handle.object_id);
        let ep_obj_id = ep_handle.object_id;
        let ep = self
            .endpoints
            .get_mut(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        ep.bind_event(event_obj_id)?;

        let event = self
            .events
            .get_mut(event_obj_id.0)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Err(e) = event.bind_endpoint(EndpointId(ep_obj_id)) {
            if let Some(ep) = self.endpoints.get_mut(ep_obj_id) {
                ep.unbind_event();
            }

            return Err(e);
        }

        let ep = self
            .endpoints
            .get(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if ep.has_pending_calls()
            && let Some(event) = self.events.get_mut(event_obj_id.0)
        {
            let woken = event.signal(Endpoint::ENDPOINT_READABLE_BIT);

            for info in woken.as_slice() {
                crate::sched::wake(self, info.thread_id, self.core_id);
            }
        }

        Ok(0)
    }

    // -- Thread lifecycle --

    fn sys_thread_create(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let entry = args[0] as usize;
        let stack_top = args[1] as usize;
        let arg = args[2] as usize;
        let space_id = self.thread_space_id(current)?;
        let thread = Thread::new(
            ThreadId(0),
            Some(space_id),
            Priority::Medium,
            entry,
            stack_top,
            arg,
        );
        let (idx, generation) = self
            .threads
            .alloc(thread)
            .ok_or(SyscallError::OutOfMemory)?;
        let t = self.threads.get_mut(idx).unwrap();

        t.id = ThreadId(idx);

        Self::init_thread_registers(t, entry, stack_top, arg);

        #[cfg(target_os = "none")]
        {
            let ks = crate::frame::arch::context::alloc_kernel_stack();

            if let Some((base, top)) = ks {
                let t = self.threads.get_mut(idx).unwrap();

                t.set_kernel_stack(base, top);
                t.init_register_state().kernel_sp = top as u64;
            } else {
                self.threads.dealloc(idx);

                return Err(SyscallError::OutOfMemory);
            }
        }

        self.link_thread_to_space(idx, space_id);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Thread, idx, Rights::ALL, generation)
        {
            Ok(hid) => {
                self.scheduler
                    .enqueue(self.core_id, ThreadId(idx), Priority::Medium);
                self.alive_threads += 1;

                Ok(hid.0 as u64)
            }
            Err(e) => {
                self.unlink_thread_from_space(idx, space_id);

                #[cfg(target_os = "none")]
                self.free_kernel_stack(idx);

                self.threads.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_thread_create_in(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let space_handle_id = HandleId(args[0] as u32);
        let entry = args[1] as usize;
        let stack_top = args[2] as usize;
        let arg = args[3] as usize;
        let handles_ptr = args[4] as usize;
        let handles_count = args[5] as usize;

        if handles_count > config::MAX_IPC_HANDLES {
            return Err(SyscallError::InvalidArgument);
        }

        let caller_space_id = self.thread_space_id(current)?;
        let space_handle = self.lookup_handle(caller_space_id, space_handle_id)?;

        if space_handle.object_type != ObjectType::AddressSpace {
            return Err(SyscallError::WrongHandleType);
        }
        if !space_handle.rights.contains(Rights::SPAWN) {
            return Err(SyscallError::InsufficientRights);
        }

        let target_space = AddressSpaceId(space_handle.object_id);
        let mut handle_ids = [0u32; config::MAX_IPC_HANDLES];

        if handles_count > 0 {
            user_mem::read_user_u32s(handles_ptr, handles_count, &mut handle_ids)?;
        }

        let thread = Thread::new(
            ThreadId(0),
            Some(target_space),
            Priority::Medium,
            entry,
            stack_top,
            arg,
        );
        let (idx, generation) = self
            .threads
            .alloc(thread)
            .ok_or(SyscallError::OutOfMemory)?;

        let t = self.threads.get_mut(idx).unwrap();

        t.id = ThreadId(idx);
        Self::init_thread_registers(t, entry, stack_top, arg);

        #[cfg(target_os = "none")]
        {
            let ks = crate::frame::arch::context::alloc_kernel_stack();

            if let Some((base, top)) = ks {
                let t = self.threads.get_mut(idx).unwrap();

                t.set_kernel_stack(base, top);
                t.init_register_state().kernel_sp = top as u64;
            } else {
                self.threads.dealloc(idx);

                return Err(SyscallError::OutOfMemory);
            }
        }

        self.link_thread_to_space(idx, target_space);

        if handles_count > 0 {
            let mut cloned = [const { None }; config::MAX_IPC_HANDLES];
            let clone_result: Result<(), SyscallError> = (|| {
                let caller_space = self
                    .spaces
                    .get(caller_space_id.0)
                    .ok_or(SyscallError::InvalidHandle)?;

                for (i, &hid) in handle_ids[..handles_count].iter().enumerate() {
                    cloned[i] = Some(caller_space.handles().lookup(HandleId(hid))?.clone());
                }

                Ok(())
            })();

            if let Err(e) = clone_result {
                self.unlink_thread_from_space(idx, target_space);

                #[cfg(target_os = "none")]
                self.free_kernel_stack(idx);

                self.threads.dealloc(idx);

                return Err(e);
            }

            let mut installed_refs = [(ObjectType::Vmo, 0u32); config::MAX_IPC_HANDLES];
            let mut installed_count = 0;

            let install_result: Result<(), SyscallError> = (|| {
                let target = self
                    .spaces
                    .get_mut(target_space.0)
                    .ok_or(SyscallError::InvalidHandle)?;

                for (i, slot) in cloned[..handles_count].iter_mut().enumerate() {
                    let h = slot.take().unwrap();

                    installed_refs[i] = (h.object_type, h.object_id);

                    if let Err(e) = target.handles_mut().allocate_at(i, h) {
                        for j in 0..i {
                            target.handles_mut().close(HandleId(j as u32)).ok();
                        }

                        return Err(e);
                    }

                    installed_count += 1;
                }

                Ok(())
            })();

            if let Err(e) = install_result {
                for &(obj_type, obj_id) in &installed_refs[..installed_count] {
                    self.release_object_ref(obj_type, obj_id);
                }

                self.unlink_thread_from_space(idx, target_space);
                #[cfg(target_os = "none")]
                self.free_kernel_stack(idx);
                self.threads.dealloc(idx);

                return Err(e);
            }

            for &(obj_type, obj_id) in &installed_refs[..installed_count] {
                self.add_object_ref(obj_type, obj_id);
            }
        }

        let space = self
            .spaces
            .get_mut(caller_space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Thread, idx, Rights::ALL, generation)
        {
            Ok(hid) => {
                let core = self.scheduler.least_loaded_core();

                self.scheduler
                    .enqueue(core, ThreadId(idx), Priority::Medium);
                self.alive_threads += 1;

                Ok(hid.0 as u64)
            }
            Err(e) => {
                self.unlink_thread_from_space(idx, target_space);
                #[cfg(target_os = "none")]
                self.free_kernel_stack(idx);
                self.threads.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_thread_exit(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let code = args[0] as u32;

        crate::sched::exit_current(self, current, self.core_id, code);

        self.alive_threads = self.alive_threads.saturating_sub(1);

        #[cfg(target_os = "none")]
        if self.alive_threads == 0 {
            crate::println!("INTEGRATION TEST: EXIT {code}");
            crate::frame::arch::psci::system_off();
        }

        Ok(0)
    }

    fn sys_thread_set_priority(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let priority_val = args[1] as u8;
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Thread {
            return Err(SyscallError::WrongHandleType);
        }

        let priority = match priority_val {
            0 => Priority::Idle,
            1 => Priority::Low,
            2 => Priority::Medium,
            3 => Priority::High,
            _ => return Err(SyscallError::InvalidArgument),
        };

        self.threads
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .set_priority(priority);

        Ok(0)
    }

    fn sys_thread_set_affinity(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let hint_val = args[1] as u8;
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Thread {
            return Err(SyscallError::WrongHandleType);
        }

        let hint = match hint_val {
            0 => crate::types::TopologyHint::Any,
            1 => crate::types::TopologyHint::Performance,
            2 => crate::types::TopologyHint::Efficiency,
            _ => return Err(SyscallError::InvalidArgument),
        };

        self.threads
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .set_affinity(hint);

        Ok(0)
    }

    // -- Space destroy --

    fn sys_space_destroy(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let caller_space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(caller_space_id, handle_id)?;

        if handle.object_type != ObjectType::AddressSpace {
            return Err(SyscallError::WrongHandleType);
        }

        let target_id = AddressSpaceId(handle.object_id);

        if target_id == caller_space_id {
            return Err(SyscallError::InvalidArgument);
        }

        if self.spaces.get(target_id.0).is_none() {
            return Err(SyscallError::InvalidHandle);
        }

        // 1. Kill all threads in the target space and remove from scheduler.
        let mut thread_cursor = self.spaces.get(target_id.0).and_then(|s| s.thread_head());

        while let Some(tid) = thread_cursor {
            thread_cursor = self.threads.get(tid).and_then(|t| t.space_next());

            let (wait_evts, wait_n) = self
                .threads
                .get_mut(tid)
                .map(|t| t.take_wait_events())
                .unwrap_or(([0; config::MAX_MULTI_WAIT], 0));

            for &evt_id in &wait_evts[..wait_n as usize] {
                if let Some(e) = self.events.get_mut(evt_id) {
                    e.remove_waiter(ThreadId(tid));
                }
            }

            for (_, ep) in self.endpoints.iter_allocated_mut() {
                ep.remove_recv_waiter(ThreadId(tid));
            }

            #[cfg(target_os = "none")]
            self.free_kernel_stack(tid);

            if let Some(t) = self.threads.get_mut(tid)
                && t.state() != crate::thread::ThreadRunState::Exited
            {
                t.exit(0);
                self.alive_threads = self.alive_threads.saturating_sub(1);
            }

            self.scheduler.remove(ThreadId(tid));
        }

        // 2. Walk the handle table and clean up referenced objects.
        // Collect into a fixed-size stack array to avoid heap allocation.
        let mut handle_buf = [(ObjectType::Vmo, 0u32); config::MAX_HANDLES];
        let mut handle_count = 0;

        if let Some(space) = self.spaces.get(target_id.0) {
            for (_, handle) in space.handles().iter_handles() {
                if handle_count < config::MAX_HANDLES {
                    handle_buf[handle_count] = (handle.object_type, handle.object_id);
                    handle_count += 1;
                }
            }
        }

        for &(obj_type, obj_id) in &handle_buf[..handle_count] {
            if obj_type == ObjectType::Endpoint
                && let Some(ep) = self.endpoints.get_mut(obj_id)
                && !ep.is_peer_closed()
            {
                self.close_endpoint_peer(obj_id);
            }

            self.release_object_ref(obj_type, obj_id);
        }

        // 3. Free page table and ASID.
        if let Some(space) = self.spaces.get(target_id.0) {
            let asid = space.asid();

            #[cfg(target_os = "none")]
            {
                let root = space.page_table_root();

                if root != 0 {
                    crate::frame::arch::page_table::destroy_page_table(
                        crate::frame::arch::page_alloc::PhysAddr(root),
                        crate::frame::arch::page_table::Asid(asid),
                    );
                }
            }

            #[cfg(all(not(target_os = "none"), test))]
            if asid != 0 {
                crate::frame::arch::page_table::free_asid(crate::frame::arch::page_table::Asid(
                    asid,
                ));
            }
        }

        // 4. Dealloc the space.
        self.spaces
            .dealloc(target_id.0)
            .ok_or(SyscallError::InvalidHandle)?;

        // 5. Close caller's handle to the destroyed space.
        let caller = self
            .spaces
            .get_mut(caller_space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let _ = caller.handles_mut().close(handle_id);

        Ok(0)
    }

    fn sys_vmo_map_into(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let vmo_handle_id = HandleId(args[0] as u32);
        let space_handle_id = HandleId(args[1] as u32);
        let addr_hint = args[2] as usize;
        let perms = Rights(args[3] as u32);
        let space_id = self.thread_space_id(current)?;
        let vmo_handle = self.lookup_handle(space_id, vmo_handle_id)?;

        if vmo_handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }
        if !vmo_handle.rights.contains(Rights::MAP) {
            return Err(SyscallError::InsufficientRights);
        }

        let space_handle = self.lookup_handle(space_id, space_handle_id)?;

        if space_handle.object_type != ObjectType::AddressSpace {
            return Err(SyscallError::WrongHandleType);
        }

        let vmo_size = self
            .vmos
            .get(vmo_handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .size();
        let target_space = self
            .spaces
            .get_mut(space_handle.object_id)
            .ok_or(SyscallError::InvalidArgument)?;
        let va = target_space.map_vmo(VmoId(vmo_handle.object_id), vmo_size, perms, addr_hint)?;

        Ok(va as u64)
    }

    fn sys_vmo_set_pager(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let vmo_handle_id = HandleId(args[0] as u32);
        let ep_handle_id = HandleId(args[1] as u32);
        let space_id = self.thread_space_id(current)?;
        let vmo_handle = self.lookup_handle(space_id, vmo_handle_id)?;

        if vmo_handle.object_type != ObjectType::Vmo {
            return Err(SyscallError::WrongHandleType);
        }
        if !vmo_handle.rights.contains(Rights::WRITE) {
            return Err(SyscallError::InsufficientRights);
        }

        let ep_handle = self.lookup_handle(space_id, ep_handle_id)?;

        if ep_handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        self.vmos
            .get_mut(vmo_handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .set_pager(EndpointId(ep_handle.object_id))?;

        Ok(0)
    }

    // -- IRQ syscalls --

    fn sys_event_bind_irq(
        &mut self,
        current: ThreadId,
        args: &[u64; 6],
    ) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let intid = args[1] as u32;
        let signal_bits = args[2];
        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;

        if handle.object_type != ObjectType::Event {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::SIGNAL) {
            return Err(SyscallError::InsufficientRights);
        }

        let event_id = EventId(handle.object_id);

        self.irqs.bind(intid, event_id, signal_bits)?;

        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;
    use crate::{thread::ThreadRunState, types::Priority};

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

        let new_vmo =
            crate::vmo::Vmo::new(crate::types::VmoId(0), 8192, crate::vmo::VmoFlags::NONE);

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

        let new_event = crate::event::Event::new(crate::types::EventId(0));

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

        let new_ep = Endpoint::new(crate::types::EndpointId(0));

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

    fn do_call(k: &mut Kernel, ep_hid: u64, msg: &[u8], reply_buf: &mut [u8; 128]) {
        reply_buf[..msg.len()].copy_from_slice(msg);

        let (err, _) = call(
            k,
            num::CALL,
            &[
                ep_hid,
                reply_buf.as_mut_ptr() as u64,
                msg.len() as u64,
                0,
                0,
                0,
            ],
        );

        assert_eq!(err, 0, "CALL failed");
    }

    fn do_recv(k: &mut Kernel, ep_hid: u64, out_buf: &mut [u8; 128]) -> (usize, u64) {
        let (err, packed) = call(
            k,
            num::RECV,
            &[ep_hid, out_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0, "RECV failed");

        let msg_len = (packed & 0xFFFF_FFFF) as usize;
        let reply_cap = packed >> 32;

        (msg_len, reply_cap)
    }

    fn do_reply(k: &mut Kernel, ep_hid: u64, reply_cap: u64, msg: &[u8]) {
        let (err, _) = call(
            k,
            num::REPLY,
            &[
                ep_hid,
                reply_cap,
                msg.as_ptr() as u64,
                msg.len() as u64,
                0,
                0,
            ],
        );

        assert_eq!(err, 0, "REPLY failed");
    }

    fn resume_caller(k: &mut Kernel) {
        if let Some(tid) = k.scheduler.pick_next(0) {
            assert_eq!(tid, ThreadId(0));

            k.threads
                .get_mut(0)
                .unwrap()
                .set_state(crate::thread::ThreadRunState::Running);
            k.scheduler.core_mut(0).set_current(Some(tid));
        }
    }

    #[test]
    fn unknown_syscall() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, 999, &[0; 6]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_create_and_close() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.vmos.count(), 1);

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_create_zero_size() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_create() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.events.count(), 1);

        let (err, _) = call(&mut k, num::EVENT_SIGNAL, &[hid, 0b101, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let event = k.events.get(0).unwrap();

        assert_eq!(event.bits(), 0b101);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_clear() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        call(&mut k, num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);
        call(&mut k, num::EVENT_CLEAR, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(k.events.get(0).unwrap().bits(), 0b10);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_create() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.endpoints.count(), 1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_create() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), 2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_dup_with_reduced_rights() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let read_only = Rights::READ.0 as u64;
        let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[hid, read_only, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_ne!(hid, dup_hid);

        let (_, info) = call(&mut k, num::HANDLE_INFO, &[dup_hid, 0, 0, 0, 0, 0]);
        let rights = (info & 0xFFFF_FFFF) as u32;

        assert_eq!(rights, Rights::READ.0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_info_returns_type_and_rights() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, info) = call(&mut k, num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let obj_type = (info >> 32) as u8;

        assert_eq!(obj_type, ObjectType::Event as u8);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_seal_through_syscall() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(k.vmos.get(0).unwrap().is_sealed());

        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::AlreadySealed as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_snapshot_through_syscall() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, snap_hid) = call(&mut k, num::VMO_SNAPSHOT, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_ne!(hid, snap_hid);
        assert_eq!(k.vmos.count(), 2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_map_and_unmap() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let (err, va) = call(&mut k, num::VMO_MAP, &[hid, 0, perms, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(va > 0);

        let (err, _) = call(&mut k, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn wrong_handle_type_rejected() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_bind_irq_and_clear_acks() {
        let mut k = setup_kernel();
        let (err, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, _) = call(
            &mut k,
            num::EVENT_BIND_IRQ,
            &[event_hid, 64, 0b1010, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let sig = k.irqs.handle_irq(64).unwrap();

        assert_eq!(sig.event_id, EventId(0));
        assert_eq!(sig.signal_bits, 0b1010);

        call(&mut k, num::EVENT_SIGNAL, &[event_hid, 0b1010, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::EVENT_CLEAR, &[event_hid, 0b1010, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_bind_irq_wrong_handle_type() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[vmo_hid, 64, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_bind_irq_invalid_intid() {
        let mut k = setup_kernel();
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[event_hid, 10, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_clear_non_irq_skips_scan() {
        let mut k = setup_kernel();
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        call(&mut k, num::EVENT_SIGNAL, &[event_hid, 0b11, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::EVENT_CLEAR, &[event_hid, 0b11, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.events.get(0).unwrap().bits(), 0);

        crate::invariants::assert_valid(&*k);
    }

    // -- New syscall tests --

    #[test]
    fn event_wait_returns_immediately_if_bits_set() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_with_upper_32_bits() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let upper_bit: u64 = 1 << 48;

        call(&mut k, num::EVENT_SIGNAL, &[hid, upper_bit, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid, upper_bit, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_with_bit_63() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let bit63: u64 = 1 << 63;

        call(&mut k, num::EVENT_SIGNAL, &[hid, bit63, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid, bit63, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_multi_wait_first_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid1, 0b1, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b1, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_multi_wait_second_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid2, 0b10, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b10, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_multi_wait_three_events_middle_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid3) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid2, 0b100, 0, 0, 0, 0]);

        let (err, value) = call(
            &mut k,
            num::EVENT_WAIT,
            &[hid1, 0b1, hid2, 0b100, hid3, 0b10],
        );

        assert_eq!(err, 0);
        assert_eq!(value, hid2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_zero_mask_skipped() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid1, 0b1, 0, 0, 0, 0]);

        let (err, value) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, 999, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_and_inspect() {
        let mut k = setup_kernel();
        let (err, _tid_handle) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.threads.count(), 2);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_set_priority() {
        let mut k = setup_kernel();
        let (_, tid_handle) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);
        let (err, _) = call(
            &mut k,
            num::THREAD_SET_PRIORITY,
            &[tid_handle, 3, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_set_affinity() {
        let mut k = setup_kernel();
        let (_, tid_handle) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);
        let (err, _) = call(
            &mut k,
            num::THREAD_SET_AFFINITY,
            &[tid_handle, 1, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_destroy() {
        let mut k = setup_kernel();
        let (err, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), 2);

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), 1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_destroy_invalid_handle() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[999, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_destroy_kills_threads() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(k.spaces.count(), 2);

        let _space_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(space_hid as u32))
            .unwrap()
            .object_id;
        let (_, _tid_hid) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, 0, 0],
        );
        let initial_threads = k.threads.count();
        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), 1);

        // Threads persist as Exited (not deallocated) because external
        // handles may still reference them. Verify the thread is Exited.
        let created_tid = initial_threads as u32 - 1;

        assert_eq!(
            k.threads.get(created_tid).unwrap().state(),
            crate::thread::ThreadRunState::Exited
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_destroy_double_returns_error() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn system_info_page_size() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 16384);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn system_info_msg_size() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 128);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn system_info_num_cores() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 1);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_set_pager() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::VMO_SET_PAGER, &[vmo_hid, ep_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_exit() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::THREAD_EXIT, &[42, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    // -- IPC message passthrough --

    fn setup_ipc_kernel() -> (Box<Kernel>, u64) {
        let mut k = setup_kernel();
        let (err, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        (k, ep_hid)
    }

    #[test]
    fn ipc_send_recv_message_bytes() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let mut call_buf = [0u8; 128];
        let request = b"hello server";

        call_buf[..request.len()].copy_from_slice(request);

        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                request.len() as u64,
                0,
                0,
                0,
            ],
        );

        assert_eq!(err, 0);

        let mut recv_buf = [0u8; 128];
        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let msg_len = (packed & 0xFFFF_FFFF) as usize;

        assert_eq!(msg_len, request.len());
        assert_eq!(&recv_buf[..msg_len], request);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_message_full_roundtrip_with_reply() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let mut call_buf = [0u8; 128];
        let request = b"request";

        call_buf[..request.len()].copy_from_slice(request);

        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                request.len() as u64,
                0,
                0,
                0,
            ],
        );

        assert_eq!(err, 0);

        let mut recv_buf = [0u8; 128];
        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let msg_len = (packed & 0xFFFF_FFFF) as usize;
        let reply_cap = (packed >> 32) as u64;

        assert_eq!(&recv_buf[..msg_len], request);

        let reply = b"response";
        let (err, _) = call(
            &mut k,
            num::REPLY,
            &[
                ep_hid,
                reply_cap,
                reply.as_ptr() as u64,
                reply.len() as u64,
                0,
                0,
            ],
        );

        assert_eq!(err, 0);
        assert_eq!(&call_buf[..reply.len()], reply);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_empty_message() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (err, _) = call(&mut k, num::CALL, &[ep_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let mut recv_buf = [0u8; 128];
        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let msg_len = (packed & 0xFFFF_FFFF) as usize;

        assert_eq!(msg_len, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_oversized_message_rejected() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let big = [0u8; 129];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, big.as_ptr() as u64, big.len() as u64, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_recv_insufficient_buffer() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let msg = b"some data here";
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, msg.as_ptr() as u64, msg.len() as u64, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let mut tiny_buf = [0u8; 4];
        let (err, _) = call(
            &mut k,
            num::RECV,
            &[ep_hid, tiny_buf.as_mut_ptr() as u64, 4, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::BufferFull as u64);

        crate::invariants::assert_valid(&*k);
    }

    // -- Handle transfer over IPC --

    #[test]
    fn ipc_transfer_single_handle() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let handle_ids = [vmo_hid as u32];
        let mut call_buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                0,
                handle_ids.as_ptr() as u64,
                1,
                0,
            ],
        );

        assert_eq!(err, 0);

        let (err, _) = k.dispatch(ThreadId(0), 0, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        let mut recv_buf = [0u8; 128];
        let mut recv_handles = [0u32; 8];
        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[
                ep_hid,
                recv_buf.as_mut_ptr() as u64,
                128,
                recv_handles.as_mut_ptr() as u64,
                8,
                0,
            ],
        );

        assert_eq!(err, 0);

        let h_count = ((packed >> 16) & 0xFFFF) as usize;

        assert_eq!(h_count, 1);

        let new_hid = recv_handles[0] as u64;
        let (err, info) = call(&mut k, num::HANDLE_INFO, &[new_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(info >> 32, ObjectType::Vmo as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_transfer_invalid_handle_fails_atomically() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let handle_ids = [vmo_hid as u32, 999u32];
        let mut call_buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                0,
                handle_ids.as_ptr() as u64,
                2,
                0,
            ],
        );

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        let (err, _) = call(&mut k, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn ipc_transfer_zero_handles() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let mut call_buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, call_buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let mut recv_buf = [0u8; 128];
        let (err, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let h_count = ((packed >> 16) & 0xFFFF) as usize;

        assert_eq!(h_count, 0);

        crate::invariants::assert_valid(&*k);
    }

    // -- Channel-event auto-signal --

    #[test]
    fn endpoint_bind_event_signals_on_enqueue() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (_, ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, ev_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let event = k.events.get(0).unwrap();

        assert_ne!(event.bits() & 1, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_no_signal_without_binding() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (_, _ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let event = k.events.get(0).unwrap();

        assert_eq!(event.bits(), 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_clear_on_drain() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (_, ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, ev_hid, 0, 0, 0, 0],
        );

        let mut buf = [0u8; 128];

        call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_ne!(k.events.get(0).unwrap().bits() & 1, 0);

        let mut recv_buf = [0u8; 128];

        call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(k.events.get(0).unwrap().bits() & 1, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_bind_with_pending_signals_immediately() {
        let (mut k, ep_hid) = setup_ipc_kernel();
        let (_, ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let mut buf = [0u8; 128];

        call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, ev_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);
        assert_ne!(k.events.get(0).unwrap().bits() & 1, 0);

        crate::invariants::assert_valid(&*k);
    }

    // -- thread_create_in with initial handles --

    #[test]
    fn thread_create_in_with_initial_handles() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let handle_ids = [vmo_hid as u32, ep_hid as u32];
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 2, handle_ids.as_ptr() as u64, 2],
        );

        assert_eq!(err, 0);

        let target_space = k.spaces.get(1).unwrap();
        let h0 = target_space.handles().lookup(HandleId(0)).unwrap();

        assert_eq!(h0.object_type, ObjectType::Vmo);

        let h1 = target_space.handles().lookup(HandleId(1)).unwrap();

        assert_eq!(h1.object_type, ObjectType::Endpoint);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_zero_handles() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, 0, 0],
        );

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_invalid_handle_rolls_back() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let handle_ids = [vmo_hid as u32, 999u32];
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, handle_ids.as_ptr() as u64, 2],
        );

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_source_handles_not_removed() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let handle_ids = [vmo_hid as u32];

        call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 1, handle_ids.as_ptr() as u64, 1],
        );

        let (err, _) = call(&mut k, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    // =====================================================================
    // Integration tests — cross-module flows
    // =====================================================================

    #[test]
    fn multiple_threads_have_distinct_priorities() {
        let mut k = setup_kernel();
        let low_thread = Thread::new(
            ThreadId(1),
            Some(AddressSpaceId(0)),
            Priority::Low,
            0x1000,
            0x2000,
            0,
        );

        k.threads.alloc(low_thread);
        k.threads.get_mut(1).unwrap().id = ThreadId(1);
        k.scheduler.enqueue(0, ThreadId(1), Priority::Low);

        let high_thread = Thread::new(
            ThreadId(2),
            Some(AddressSpaceId(0)),
            Priority::High,
            0x1000,
            0x2000,
            0,
        );

        k.threads.alloc(high_thread);
        k.threads.get_mut(2).unwrap().id = ThreadId(2);
        k.scheduler.enqueue(0, ThreadId(2), Priority::High);

        assert_eq!(
            k.threads.get(0).unwrap().effective_priority(),
            Priority::Medium
        );
        assert_eq!(
            k.threads.get(1).unwrap().effective_priority(),
            Priority::Low
        );
        assert_eq!(
            k.threads.get(2).unwrap().effective_priority(),
            Priority::High
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_snapshot_cow_fault_resolution() {
        use crate::fault;

        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[16384, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let rw_rights: u64 = (Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0) as u64;
        let (err, va) = call(&mut k, num::VMO_MAP, &[vmo_hid, 0, rw_rights, 0, 0, 0]);

        assert_eq!(err, 0);

        // Trigger lazy allocation to commit the page.
        let action = fault::handle_data_abort(&mut k, ThreadId(0), va as usize, true);

        assert_eq!(action, fault::FaultAction::Resolved);

        // The VMO should now have a committed page.
        let vmo_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_hid as u32))
            .unwrap()
            .object_id;

        assert!(k.vmos.get(vmo_obj_id).unwrap().page_at(0).is_some());

        // Snapshot the VMO.
        let (err, snap_hid) = call(&mut k, num::VMO_SNAPSHOT, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Map the snapshot.
        let (err, snap_va) = call(&mut k, num::VMO_MAP, &[snap_hid, 0, rw_rights, 0, 0, 0]);

        assert_eq!(err, 0);

        // The snapshot should share the same page.
        let snap_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(snap_hid as u32))
            .unwrap()
            .object_id;
        let original_page = k.vmos.get(snap_obj_id).unwrap().page_at(0).unwrap();
        // Write fault on the snapshot triggers COW.
        let action = fault::handle_data_abort(&mut k, ThreadId(0), snap_va as usize, true);

        assert_eq!(action, fault::FaultAction::Resolved);

        // After COW, the snapshot should have a DIFFERENT physical page.
        let cow_page = k.vmos.get(snap_obj_id).unwrap().page_at(0).unwrap();

        assert_ne!(
            original_page, cow_page,
            "COW should allocate a new page, not keep the shared one"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_multiple_events_first_fires() {
        let mut k = setup_kernel();
        let (err1, ev1_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err2, ev2_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err1, 0);
        assert_eq!(err2, 0);

        // Signal event 1 before waiting — should immediately return.
        let (err, _) = call(&mut k, num::EVENT_SIGNAL, &[ev1_hid, 0x1, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Wait on event 2 first (not ready), then event 1 (ready).
        // Register-path: args[0]=ev2_handle, args[1]=mask, args[2]=ev1_handle, args[3]=mask
        let (err, val) = call(
            &mut k,
            num::EVENT_WAIT,
            &[ev2_hid, 0xFF, ev1_hid, 0xFF, 0, 0],
        );

        assert_eq!(err, 0);
        assert_eq!(val, ev1_hid);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_create_and_thread_create_in() {
        let mut k = setup_kernel();
        let (err, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[16384, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Create a thread in the new space with one transferred handle.
        let handle_ids = [vmo_hid as u32];
        let (err, thread_hid) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 1, handle_ids.as_ptr() as u64, 1],
        );

        assert_eq!(err, 0);
        assert!(thread_hid > 0);

        // Verify the original VMO handle still exists in the source space.
        let (err, _) = call(&mut k, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_dup_respects_right_reduction() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[16384, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Dup with reduced rights (read-only).
        let read_only = Rights::READ.0 as u64;
        let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[vmo_hid, read_only, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // The dup handle should have reduced rights.
        // HANDLE_INFO returns (object_type << 32) | rights.
        let (err, info) = call(&mut k, num::HANDLE_INFO, &[dup_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let dup_rights = info as u32;

        assert_eq!(dup_rights & Rights::READ.0, Rights::READ.0);
        assert_eq!(dup_rights & Rights::WRITE.0, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn close_handle_then_use_returns_error() {
        let mut k = setup_kernel();
        let (err, ev_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[ev_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Using the closed handle should fail.
        let (err, _) = call(&mut k, num::EVENT_SIGNAL, &[ev_hid, 0x1, 0, 0, 0, 0]);

        assert_ne!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn seal_prevents_write_fault() {
        use crate::fault;

        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[16384, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Seal the VMO.
        let (err, _) = call(&mut k, num::VMO_SEAL, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Map it writable (the mapping has write rights, but the VMO is sealed).
        let rw_rights: u64 = (Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0) as u64;
        let (err, va) = call(&mut k, num::VMO_MAP, &[vmo_hid, 0, rw_rights, 0, 0, 0]);

        assert_eq!(err, 0);

        // Write fault on sealed VMO should kill.
        let action = fault::handle_data_abort(&mut k, ThreadId(0), va as usize, true);

        assert_eq!(action, fault::FaultAction::Kill);

        crate::invariants::assert_valid(&*k);
    }

    // =====================================================================
    // Adversarial input tests — malicious userspace arguments
    // =====================================================================

    #[test]
    fn adversarial_vmo_create_size_max() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::VMO_CREATE, &[u64::MAX, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_vmo_create_unknown_flags() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::VMO_CREATE, &[4096, 0xDEAD_BEEF, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_vmo_resize_to_zero() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_RESIZE, &[hid, 0, 0, 0, 0, 0]);

        // Resize to 0 is permitted by the handler (only > MAX_PHYS_MEM is rejected).
        // Verify it succeeds without panic.
        assert_eq!(err, 0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_vmo_resize_exceeds_max_phys_mem() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let too_big = (config::MAX_PHYS_MEM as u64) + 1;
        let (err, _) = call(&mut k, num::VMO_RESIZE, &[hid, too_big, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_vmo_map_invalid_handle_max() {
        let mut k = setup_kernel();
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let (err, _) = call(&mut k, num::VMO_MAP, &[u32::MAX as u64, 0, perms, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_handle_dup_rights_escalation() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Dup with only READ rights.
        let read_only = Rights::READ.0 as u64;
        let (err, dup_hid) = call(&mut k, num::HANDLE_DUP, &[hid, read_only, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        // Now try to dup the read-only handle requesting u32::MAX rights.
        let (err, _) = call(
            &mut k,
            num::HANDLE_DUP,
            &[dup_hid, u32::MAX as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InsufficientRights as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_thread_set_priority_out_of_range() {
        let mut k = setup_kernel();
        let (err, tid_handle) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(
            &mut k,
            num::THREAD_SET_PRIORITY,
            &[tid_handle, 255, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_event_signal_on_non_event() {
        let mut k = setup_kernel();
        let (err, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::EVENT_SIGNAL, &[vmo_hid, 0b1, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_event_wait_buffer_path_garbage_pointer() {
        let mut k = setup_kernel();
        // args[0] > MAX_HANDLES triggers the buffer path, which interprets
        // args[0] as a user pointer and args[1] as count. A count exceeding
        // MAX_MULTI_WAIT must be rejected before any pointer dereference.
        let garbage_ptr = (config::MAX_HANDLES as u64) + 1;
        let bad_count = (config::MAX_MULTI_WAIT as u64) + 1;
        let (err, _) = call(
            &mut k,
            num::EVENT_WAIT,
            &[garbage_ptr, bad_count, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_event_wait_buffer_path_zero_count() {
        let mut k = setup_kernel();
        // Buffer path with count=0 must also be rejected.
        let garbage_ptr = (config::MAX_HANDLES as u64) + 1;
        let (err, _) = call(&mut k, num::EVENT_WAIT, &[garbage_ptr, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_handle_close_invalid() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[999, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_handle_close_u32_max() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[u32::MAX as u64, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidHandle as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn adversarial_unknown_syscall_high() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, u64::MAX, &[0; 6]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn kernel_init_heap_budget() {
        let k = Box::new(Kernel::new(1));
        let entry_overhead = core::mem::size_of::<Option<Box<u8>>>()
            * (config::MAX_VMOS
                + config::MAX_EVENTS
                + config::MAX_ENDPOINTS
                + config::MAX_THREADS
                + config::MAX_ADDRESS_SPACES);
        let metadata_per_slot = core::mem::size_of::<core::sync::atomic::AtomicU64>()
            + core::mem::size_of::<core::sync::atomic::AtomicU32>();
        let metadata_overhead = metadata_per_slot
            * (config::MAX_VMOS
                + config::MAX_EVENTS
                + config::MAX_ENDPOINTS
                + config::MAX_THREADS
                + config::MAX_ADDRESS_SPACES);
        let total_init_bytes = entry_overhead + metadata_overhead;
        let heap_limit = 4 * 1024 * 1024;

        assert!(
            total_init_bytes < heap_limit / 2,
            "Kernel init heap ({total_init_bytes} bytes) exceeds 50% of bare-metal heap ({heap_limit} bytes). \
             ObjectTable entries must use Box<T> for lazy allocation."
        );

        crate::invariants::assert_valid(&*k);
    }

    // =====================================================================
    // Per-syscall error path tests
    // =====================================================================

    #[test]
    fn vmo_map_wrong_type() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_err(
            call(
                &mut k,
                num::VMO_MAP,
                &[evt, 0, Rights::READ.0 as u64, 0, 0, 0],
            ),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_no_map_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let no_map = dup_with_rights(&mut k, vmo, Rights::READ.0 | Rights::WRITE.0);

        assert_err(
            call(
                &mut k,
                num::VMO_MAP,
                &[no_map, 0, Rights::READ.0 as u64, 0, 0, 0],
            ),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_no_write_right_but_requests_write() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let ro = dup_with_rights(&mut k, vmo, Rights::READ.0 | Rights::MAP.0);
        let perms = (Rights::READ.0 | Rights::WRITE.0) as u64;

        assert_err(
            call(&mut k, num::VMO_MAP, &[ro, 0, perms, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_generation_mismatch() {
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
    fn vmo_map_into_wrong_type_for_vmo() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);
        let space = create_space(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[evt, space, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_into_wrong_type_for_space() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[vmo, evt, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_into_no_map_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let no_map = dup_with_rights(&mut k, vmo, Rights::READ.0);
        let space = create_space(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[no_map, space, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_map_into_generation_mismatch_vmo() {
        let mut k = setup_kernel();
        let stale_vmo = create_stale_vmo_handle(&mut k);
        let space = create_space(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[stale_vmo, space, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_map_into_generation_mismatch_space() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let stale_space = create_stale_space_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_MAP_INTO, &[vmo, stale_space, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_unmap_nonexistent_address() {
        let mut k = setup_kernel();

        assert_err(
            call(&mut k, num::VMO_UNMAP, &[0xDEAD_0000, 0, 0, 0, 0, 0]),
            SyscallError::NotFound,
        );

        inv(&k);
    }

    #[test]
    fn vmo_unmap_double() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let va = assert_ok(call(&mut k, num::VMO_MAP, &[vmo, 0, perms, 0, 0, 0]));

        assert_ok(call(&mut k, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]),
            SyscallError::NotFound,
        );

        inv(&k);
    }

    #[test]
    fn vmo_snapshot_wrong_type() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::VMO_SNAPSHOT, &[evt, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_snapshot_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_SNAPSHOT, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_snapshot_rollback_on_handle_table_full() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        loop {
            if call(&mut k, num::EVENT_CREATE, &[0; 6]).0 != 0 {
                break;
            }
        }

        let vmo_count_before = k.vmos.count();

        assert_err(
            call(&mut k, num::VMO_SNAPSHOT, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::OutOfMemory,
        );

        assert_eq!(k.vmos.count(), vmo_count_before, "Snapshot VMO leaked");

        inv(&k);
    }

    #[test]
    fn vmo_seal_wrong_type() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::VMO_SEAL, &[evt, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_seal_no_write_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let ro = dup_with_rights(&mut k, vmo, Rights::READ.0);

        assert_err(
            call(&mut k, num::VMO_SEAL, &[ro, 0, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_seal_double() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::AlreadySealed,
        );

        inv(&k);
    }

    #[test]
    fn vmo_seal_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_SEAL, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_resize_happy_path_up_and_down() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_RESIZE, &[vmo, 8192, 0, 0, 0, 0]));

        assert_eq!(k.vmos.get(0).unwrap().size(), 8192);

        assert_ok(call(&mut k, num::VMO_RESIZE, &[vmo, 4096, 0, 0, 0, 0]));

        assert_eq!(k.vmos.get(0).unwrap().size(), 4096);

        inv(&k);
    }

    #[test]
    fn vmo_resize_sealed() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::VMO_RESIZE, &[vmo, 8192, 0, 0, 0, 0]),
            SyscallError::AlreadySealed,
        );

        inv(&k);
    }

    #[test]
    fn vmo_resize_wrong_type() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::VMO_RESIZE, &[evt, 4096, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_resize_no_write_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let ro = dup_with_rights(&mut k, vmo, Rights::READ.0);

        assert_err(
            call(&mut k, num::VMO_RESIZE, &[ro, 8192, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_resize_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_RESIZE, &[stale, 4096, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_set_pager_wrong_type_vmo() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);
        let ep = create_endpoint(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[evt, ep, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_set_pager_wrong_type_endpoint() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[vmo, evt, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn vmo_set_pager_no_write_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let ro = dup_with_rights(&mut k, vmo, Rights::READ.0);
        let ep = create_endpoint(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[ro, ep, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn vmo_set_pager_generation_mismatch_vmo() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);
        let ep = create_endpoint(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[stale, ep, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn vmo_set_pager_generation_mismatch_endpoint() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::VMO_SET_PAGER, &[vmo, stale, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn call_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::CALL, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn call_too_many_handles() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let too_many = (config::MAX_IPC_HANDLES + 1) as u64;

        assert_err(
            call(&mut k, num::CALL, &[ep, 0, 0, 0, too_many, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn call_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::CALL, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn recv_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::RECV, &[vmo, 0, 128, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn recv_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::RECV, &[stale, 0, 128, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn recv_peer_closed() {
        let mut k = setup_kernel();
        let ep_hid = create_endpoint(&mut k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        k.endpoints.get_mut(obj_id).unwrap().close_peer();

        let mut buf = [0u8; 128];

        assert_err(
            call(
                &mut k,
                num::RECV,
                &[ep_hid, buf.as_mut_ptr() as u64, 128, 0, 0, 0],
            ),
            SyscallError::PeerClosed,
        );

        inv(&k);
    }

    #[test]
    fn reply_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::REPLY, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn reply_too_many_handles() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let too_many = (config::MAX_IPC_HANDLES + 1) as u64;

        assert_err(
            call(&mut k, num::REPLY, &[ep, 0, 0, 0, 0, too_many]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn reply_invalid_reply_cap() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        assert_err(
            call(&mut k, num::REPLY, &[ep, 9999, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        inv(&k);
    }

    #[test]
    fn reply_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);

        assert_err(
            call(&mut k, num::REPLY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn event_signal_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_SIGNAL, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn event_wait_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::EVENT_WAIT, &[vmo, 0b1, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn event_wait_no_wait_right() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);
        let no_wait = dup_with_rights(&mut k, evt, Rights::READ.0 | Rights::SIGNAL.0);

        call(&mut k, num::EVENT_SIGNAL, &[evt, 0b1, 0, 0, 0, 0]);

        assert_err(
            call(&mut k, num::EVENT_WAIT, &[no_wait, 0b1, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn event_wait_all_masks_zero() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::EVENT_WAIT, &[evt, 0, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn event_wait_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_WAIT, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn event_clear_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::EVENT_CLEAR, &[vmo, 0b1, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn event_clear_no_signal_right() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);
        let ro = dup_with_rights(&mut k, evt, Rights::READ.0);

        assert_err(
            call(&mut k, num::EVENT_CLEAR, &[ro, 0b1, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn event_clear_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_CLEAR, &[stale, 0b1, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn thread_create_in_wrong_type_space() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(
                &mut k,
                num::THREAD_CREATE_IN,
                &[vmo, 0x1000, 0x2000, 0, 0, 0],
            ),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn thread_create_in_no_spawn_right() {
        let mut k = setup_kernel();
        let space = create_space(&mut k);
        let no_spawn = dup_with_rights(&mut k, space, Rights::READ.0);

        assert_err(
            call(
                &mut k,
                num::THREAD_CREATE_IN,
                &[no_spawn, 0x1000, 0x2000, 0, 0, 0],
            ),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn thread_create_in_too_many_handles() {
        let mut k = setup_kernel();
        let space = create_space(&mut k);
        let too_many = (config::MAX_IPC_HANDLES + 1) as u64;

        assert_err(
            call(
                &mut k,
                num::THREAD_CREATE_IN,
                &[space, 0x1000, 0x2000, 0, 0, too_many],
            ),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn thread_create_in_generation_mismatch() {
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
    fn thread_set_priority_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_PRIORITY, &[vmo, 2, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn thread_set_priority_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_thread_handle(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_PRIORITY, &[stale, 2, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn thread_set_affinity_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn thread_set_affinity_invalid_value() {
        let mut k = setup_kernel();
        let thr = create_thread(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[thr, 3, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );
        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[thr, 255, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn thread_set_affinity_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_thread_handle(&mut k);

        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn space_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES {
            if call(&mut k, num::EVENT_CREATE, &[0; 6]).0 != 0 {
                break;
            }
        }

        let space_count_before = k.spaces.count();
        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_ne!(err, 0);
        assert_eq!(k.spaces.count(), space_count_before, "Space leaked");

        inv(&k);
    }

    #[test]
    fn space_destroy_wrong_type() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_err(
            call(&mut k, num::SPACE_DESTROY, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::WrongHandleType,
        );

        inv(&k);
    }

    #[test]
    fn space_destroy_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_space_handle(&mut k);

        assert_err(
            call(&mut k, num::SPACE_DESTROY, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn handle_dup_no_dup_right() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let no_dup = dup_with_rights(&mut k, vmo, Rights::READ.0);

        assert_err(
            call(
                &mut k,
                num::HANDLE_DUP,
                &[no_dup, Rights::READ.0 as u64, 0, 0, 0, 0],
            ),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn handle_dup_generation_mismatch() {
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
    fn handle_dup_table_full() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        loop {
            if call(&mut k, num::EVENT_CREATE, &[0; 6]).0 != 0 {
                break;
            }
        }

        assert_err(
            call(
                &mut k,
                num::HANDLE_DUP,
                &[vmo, Rights::ALL.0 as u64, 0, 0, 0, 0],
            ),
            SyscallError::OutOfMemory,
        );

        inv(&k);
    }

    #[test]
    fn handle_info_invalid() {
        let mut k = setup_kernel();

        assert_err(
            call(&mut k, num::HANDLE_INFO, &[999, 0, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        inv(&k);
    }

    #[test]
    fn handle_info_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_vmo_handle(&mut k);

        assert_err(
            call(&mut k, num::HANDLE_INFO, &[stale, 0, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn event_bind_irq_no_signal_right() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);
        let ro = dup_with_rights(&mut k, evt, Rights::READ.0 | Rights::WAIT.0);

        assert_err(
            call(&mut k, num::EVENT_BIND_IRQ, &[ro, 32, 0b1, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn event_bind_irq_generation_mismatch() {
        let mut k = setup_kernel();
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::EVENT_BIND_IRQ, &[stale, 32, 0b1, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn event_bind_irq_double_bind_same_intid() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        assert_ok(call(&mut k, num::EVENT_BIND_IRQ, &[evt, 32, 0b1, 0, 0, 0]));
        assert_err(
            call(&mut k, num::EVENT_BIND_IRQ, &[evt, 32, 0b10, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn endpoint_bind_event_no_write_right_on_endpoint() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let ro_ep = dup_with_rights(&mut k, ep, Rights::READ.0);
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::ENDPOINT_BIND_EVENT, &[ro_ep, evt, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn endpoint_bind_event_no_signal_right_on_event() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let evt = create_event(&mut k);
        let ro_evt = dup_with_rights(&mut k, evt, Rights::READ.0 | Rights::WAIT.0);

        assert_err(
            call(&mut k, num::ENDPOINT_BIND_EVENT, &[ep, ro_evt, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn endpoint_bind_event_generation_mismatch_endpoint() {
        let mut k = setup_kernel();
        let stale = create_stale_endpoint_handle(&mut k);
        let evt = create_event(&mut k);

        assert_err(
            call(&mut k, num::ENDPOINT_BIND_EVENT, &[stale, evt, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn endpoint_bind_event_generation_mismatch_event() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let stale = create_stale_event_handle(&mut k);

        assert_err(
            call(&mut k, num::ENDPOINT_BIND_EVENT, &[ep, stale, 0, 0, 0, 0]),
            SyscallError::GenerationMismatch,
        );
    }

    #[test]
    fn clock_read_returns_value() {
        let mut k = setup_kernel();
        let (err, _val) = call(&mut k, num::CLOCK_READ, &[0; 6]);

        assert_eq!(err, 0);

        inv(&k);
    }

    #[test]
    fn system_info_invalid_selector() {
        let mut k = setup_kernel();

        assert_err(
            call(&mut k, num::SYSTEM_INFO, &[3, 0, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );
        assert_err(
            call(&mut k, num::SYSTEM_INFO, &[u64::MAX, 0, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    // =====================================================================
    // Multi-round IPC tests
    // =====================================================================

    #[test]
    fn ipc_ping_pong_10_rounds() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for round in 0..10u8 {
            let request = [b'P', b'I', b'N', b'G', round];

            let mut reply_buf = [0u8; 128];

            do_call(&mut k, ep, &request, &mut reply_buf);

            let mut recv_buf = [0u8; 128];
            let (msg_len, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            assert_eq!(
                &recv_buf[..msg_len],
                &request,
                "round {round}: data mismatch"
            );

            let response = [b'P', b'O', b'N', b'G', round];

            do_reply(&mut k, ep, reply_cap, &response);
            resume_caller(&mut k);
        }

        inv(&k);
    }

    #[test]
    fn ipc_ping_pong_100_rounds() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for round in 0..100u16 {
            let request = round.to_le_bytes();
            let mut call_reply_buf = [0u8; 128];

            do_call(&mut k, ep, &request, &mut call_reply_buf);

            let mut recv_buf = [0u8; 128];
            let (msg_len, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            assert_eq!(msg_len, 2, "round {round}");
            assert_eq!(
                u16::from_le_bytes([recv_buf[0], recv_buf[1]]),
                round,
                "round {round}: data mismatch"
            );

            let response = (!round).to_le_bytes();

            do_reply(&mut k, ep, reply_cap, &response);
            resume_caller(&mut k);
        }

        inv(&k);
    }

    #[test]
    fn ipc_ping_pong_data_integrity_varied_sizes() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for size in [0, 1, 2, 4, 8, 16, 32, 64, 127, 128] {
            let request: alloc::vec::Vec<u8> = (0..size).map(|i| (i & 0xFF) as u8).collect();

            let mut call_reply_buf = [0u8; 128];

            do_call(&mut k, ep, &request, &mut call_reply_buf);

            let mut recv_buf = [0u8; 128];
            let (msg_len, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            assert_eq!(msg_len, size, "size {size}");
            assert_eq!(
                &recv_buf[..msg_len],
                &request[..],
                "size {size}: data corrupt"
            );

            do_reply(&mut k, ep, reply_cap, &[]);
            resume_caller(&mut k);
        }

        inv(&k);
    }

    #[test]
    fn ipc_many_callers_then_drain() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep as u32))
            .unwrap()
            .object_id;
        let priorities = [
            Priority::Low,
            Priority::Low,
            Priority::Low,
            Priority::Low,
            Priority::Medium,
            Priority::Medium,
            Priority::Medium,
            Priority::Medium,
            Priority::High,
            Priority::High,
            Priority::High,
            Priority::High,
        ];
        let n = priorities.len();
        let mut reply_bufs: [([u8; 128], ThreadId); 12] =
            core::array::from_fn(|_| ([0u8; 128], ThreadId(0)));

        for i in 0..n {
            let thr = Thread::new(ThreadId(0), Some(AddressSpaceId(0)), priorities[i], 0, 0, 0);
            let (tid, _) = k.threads.alloc(thr).unwrap();

            k.threads.get_mut(tid).unwrap().id = ThreadId(tid);
            k.threads
                .get_mut(tid)
                .unwrap()
                .set_state(crate::thread::ThreadRunState::Blocked);
            reply_bufs[i].1 = ThreadId(tid);

            let endpoint = k.endpoints.get_mut(ep_obj_id).unwrap();
            let pending_call = PendingCall {
                caller: ThreadId(tid),
                priority: priorities[i],
                message: crate::endpoint::Message::from_bytes(&[i as u8]).unwrap(),
                handles: [const { None }; config::MAX_IPC_HANDLES],
                handle_count: 0,
                badge: i as u32,
                reply_buf: reply_bufs[i].0.as_mut_ptr() as usize,
            };

            endpoint.enqueue_call(pending_call).unwrap();
        }
        for i in 0..n {
            let mut recv_buf = [0u8; 128];
            let (err, packed) = call(
                &mut k,
                num::RECV,
                &[ep, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
            );

            assert_eq!(err, 0, "recv {i} failed");

            let msg_len = (packed & 0xFFFF_FFFF) as usize;
            let reply_cap = packed >> 32;

            assert_eq!(msg_len, 1, "recv {i}: wrong length");

            do_reply(&mut k, ep, reply_cap, &[]);
        }

        assert_eq!(
            k.endpoints.get(ep_obj_id).unwrap().pending_call_count(),
            0,
            "pending calls not drained"
        );
        inv(&k);
    }

    #[test]
    fn ipc_interleaved_call_recv_reply_cycles() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for round in 0..20u8 {
            let msg = [round; 4];
            let mut call_reply_buf = [0u8; 128];

            do_call(&mut k, ep, &msg, &mut call_reply_buf);

            let mut recv_buf = [0u8; 128];
            let (msg_len, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            assert_eq!(msg_len, 4);
            assert_eq!(recv_buf[0], round, "round {round}: first byte wrong");
            assert_eq!(recv_buf[3], round, "round {round}: last byte wrong");

            do_reply(&mut k, ep, reply_cap, &[!round; 4]);
            resume_caller(&mut k);
        }

        assert_eq!(
            k.endpoints.get(0).unwrap().pending_call_count(),
            0,
            "pending calls leaked"
        );
        assert_eq!(
            k.endpoints.get(0).unwrap().pending_reply_count(),
            0,
            "reply caps leaked"
        );

        inv(&k);
    }

    #[test]
    fn ipc_reply_cap_not_reusable() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        let mut call_reply_buf = [0u8; 128];

        do_call(&mut k, ep, b"first", &mut call_reply_buf);

        let mut recv_buf = [0u8; 128];
        let (_, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

        do_reply(&mut k, ep, reply_cap, b"ok");
        resume_caller(&mut k);

        assert_err(
            call(&mut k, num::REPLY, &[ep, reply_cap, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        inv(&k);
    }

    #[test]
    fn ipc_different_endpoints_independent() {
        let mut k = setup_kernel();
        let ep1 = create_endpoint(&mut k);
        let ep2 = create_endpoint(&mut k);

        let mut reply1 = [0u8; 128];

        do_call(&mut k, ep1, b"ep1-msg", &mut reply1);

        let mut buf1 = [0u8; 128];
        let (len1, rc1) = do_recv(&mut k, ep1, &mut buf1);

        assert_eq!(&buf1[..len1], b"ep1-msg");

        do_reply(&mut k, ep1, rc1, b"r1");
        resume_caller(&mut k);

        let mut reply2 = [0u8; 128];

        do_call(&mut k, ep2, b"ep2-msg", &mut reply2);

        let mut buf2 = [0u8; 128];
        let (len2, rc2) = do_recv(&mut k, ep2, &mut buf2);

        assert_eq!(&buf2[..len2], b"ep2-msg");

        do_reply(&mut k, ep2, rc2, b"r2");
        resume_caller(&mut k);
        inv(&k);
    }

    #[test]
    fn ipc_handle_transfer_per_round() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for round in 0..5 {
            let vmo = create_vmo(&mut k);
            let mut call_buf = [0u8; 128];
            let handles = [vmo as u32, 0, 0, 0, 0, 0, 0, 0];
            let (err, _) = call(
                &mut k,
                num::CALL,
                &[
                    ep,
                    call_buf.as_mut_ptr() as u64,
                    0,
                    handles.as_ptr() as u64,
                    1,
                    0,
                ],
            );

            assert_eq!(err, 0, "round {round}: call failed");
            assert_eq!(
                call(&mut k, num::HANDLE_INFO, &[vmo, 0, 0, 0, 0, 0]).0,
                SyscallError::InvalidHandle as u64,
                "round {round}: transferred handle still valid"
            );

            let mut recv_buf = [0u8; 128];
            let mut handles_out = [0u32; 8];
            let (err, packed) = call(
                &mut k,
                num::RECV,
                &[
                    ep,
                    recv_buf.as_mut_ptr() as u64,
                    128,
                    handles_out.as_mut_ptr() as u64,
                    8,
                    0,
                ],
            );

            assert_eq!(err, 0, "round {round}: recv failed");

            let reply_cap = packed >> 32;
            let handle_count = ((packed >> 16) & 0xFFFF) as usize;

            assert_eq!(handle_count, 1, "round {round}: wrong handle count");

            let received_hid = handles_out[0] as u64;
            let (err, info) = call(&mut k, num::HANDLE_INFO, &[received_hid, 0, 0, 0, 0, 0]);

            assert_eq!(err, 0, "round {round}: received handle invalid");
            assert_eq!(
                (info >> 32) as u8,
                ObjectType::Vmo as u8,
                "round {round}: wrong type"
            );

            do_reply(&mut k, ep, reply_cap, &[]);
            resume_caller(&mut k);
        }

        inv(&k);
    }

    #[test]
    fn endpoint_bound_event_ping_pong() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let evt = create_event(&mut k);

        assert_ok(call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep, evt, 0, 0, 0, 0],
        ));

        for round in 0..5 {
            let mut call_reply_buf = [0u8; 128];

            do_call(&mut k, ep, &[round as u8], &mut call_reply_buf);

            let bits = k.events.get(0).unwrap().bits();

            assert_ne!(bits, 0, "round {round}: event not signaled after call");

            let mut recv_buf = [0u8; 128];
            let (_, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            do_reply(&mut k, ep, reply_cap, &[]);
            resume_caller(&mut k);
        }

        inv(&k);
    }

    // -- Stress tests --

    #[test]
    fn handle_churn_stress_100_cycles() {
        let mut k = setup_kernel();

        for _ in 0..100 {
            let h = create_vmo(&mut k);

            call(&mut k, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]);
        }

        inv(&k);
    }

    #[test]
    fn space_destroy_with_mapped_vmos_and_threads() {
        let mut k = setup_kernel();
        let space_h = create_space(&mut k);
        let vmo_h = create_vmo(&mut k);
        let vmo_obj = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_h as u32))
            .unwrap()
            .object_id;

        k.vmos.get_mut(vmo_obj).unwrap().add_ref();

        assert_ok(call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_h, 0x1000, 0x2000, 0, 0, 0],
        ));
        assert_ok(call(&mut k, num::SPACE_DESTROY, &[space_h, 0, 0, 0, 0, 0]));

        inv(&k);
    }

    #[test]
    fn ipc_stress_16_callers_drain() {
        let mut k = setup_kernel();
        let _ep = create_endpoint(&mut k);

        for i in 0..16 {
            let t = Thread::new(
                ThreadId(0),
                Some(AddressSpaceId(0)),
                Priority::Medium,
                0x1000,
                0x2000,
                0,
            );
            let (idx, _) = k.threads.alloc(t).unwrap();

            k.threads.get_mut(idx).unwrap().id = ThreadId(idx);
            k.threads
                .get_mut(idx)
                .unwrap()
                .set_state(crate::thread::ThreadRunState::Blocked);

            let msg = [i as u8; 4];
            let msg_obj = crate::endpoint::Message::from_bytes(&msg).unwrap();
            let pending = crate::endpoint::PendingCall {
                caller: ThreadId(idx),
                priority: Priority::Medium,
                message: msg_obj,
                handles: [const { None }; config::MAX_IPC_HANDLES],
                handle_count: 0,
                badge: 0,
                reply_buf: 0,
            };

            k.endpoints.get_mut(0).unwrap().enqueue_call(pending).ok();
        }

        for _ in 0..16 {
            let ep_obj = k.endpoints.get_mut(0).unwrap();

            if let Some((call, reply_cap)) = ep_obj.dequeue_call() {
                ep_obj.consume_reply(reply_cap).ok();

                crate::sched::wake(&mut k, call.caller, 0);
            }
        }

        inv(&k);
    }

    #[test]
    fn mixed_object_lifecycle_stress() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();

        for i in 0..20 {
            match i % 4 {
                0 => handles.push(create_vmo(&mut k)),
                1 => handles.push(create_event(&mut k)),
                2 => handles.push(create_endpoint(&mut k)),
                3 => handles.push(create_thread(&mut k)),
                _ => unreachable!(),
            }
        }

        for h in handles.iter().rev() {
            call(&mut k, num::HANDLE_CLOSE, &[*h, 0, 0, 0, 0, 0]);
        }

        inv(&k);

        for i in 0..20 {
            match i % 4 {
                0 => {
                    create_vmo(&mut k);
                }
                1 => {
                    create_event(&mut k);
                }
                2 => {
                    create_endpoint(&mut k);
                }
                3 => {
                    create_thread(&mut k);
                }
                _ => unreachable!(),
            }
        }

        inv(&k);
    }

    #[test]
    fn scheduler_round_robin_fairness() {
        let mut k = Box::new(Kernel::new(1));
        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        k.spaces.alloc(space);

        let mut tids = alloc::vec::Vec::new();

        for _ in 0..8 {
            let t = Thread::new(
                ThreadId(0),
                Some(AddressSpaceId(0)),
                Priority::Medium,
                0x1000,
                0x2000,
                0,
            );
            let (idx, _) = k.threads.alloc(t).unwrap();

            k.threads.get_mut(idx).unwrap().id = ThreadId(idx);
            k.scheduler.enqueue(0, ThreadId(idx), Priority::Medium);
            tids.push(ThreadId(idx));
        }

        let mut order = alloc::vec::Vec::new();

        for _ in 0..8 {
            let next = k.scheduler.pick_next(0).unwrap();

            order.push(next);
        }

        assert_eq!(order, tids);
    }

    #[test]
    fn vmo_resize_to_zero_through_syscall() {
        let mut k = setup_kernel();
        let h = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_RESIZE, &[h, 0, 0, 0, 0, 0]));

        inv(&k);
    }

    // ── Convergence pass 1: adversarial tests ──

    #[test]
    fn adversarial_every_syscall_with_all_zero_args() {
        let mut k = setup_kernel();

        for num in 0..=30 {
            if num == num::THREAD_EXIT {
                continue;
            }

            let _ = call(&mut k, num, &[0; 6]);
        }

        inv(&k);
    }

    #[test]
    fn adversarial_every_syscall_with_all_max_args() {
        let mut k = setup_kernel();

        for num in 0..=30 {
            if num == num::THREAD_EXIT {
                continue;
            }

            let _ = call(&mut k, num, &[u64::MAX; 6]);
        }

        inv(&k);
    }

    #[test]
    fn adversarial_vmo_create_page_size_minus_one() {
        let mut k = setup_kernel();
        let r = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64 - 1, 0, 0, 0, 0, 0],
        );

        assert_eq!(r.0, 0);

        inv(&k);
    }

    #[test]
    fn adversarial_vmo_create_exactly_max_phys_mem() {
        let mut k = setup_kernel();

        assert_err(
            call(
                &mut k,
                num::VMO_CREATE,
                &[config::MAX_PHYS_MEM as u64 + 1, 0, 0, 0, 0, 0],
            ),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_double_close_same_handle() {
        let mut k = setup_kernel();
        let h = create_vmo(&mut k);

        assert_ok(call(&mut k, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::HANDLE_CLOSE, &[h, 0, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_use_after_close_all_typed_syscalls() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let event = create_event(&mut k);
        let ep = create_endpoint(&mut k);

        call(&mut k, num::HANDLE_CLOSE, &[vmo, 0, 0, 0, 0, 0]);
        call(&mut k, num::HANDLE_CLOSE, &[event, 0, 0, 0, 0, 0]);
        call(&mut k, num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);

        assert_err(
            call(&mut k, num::VMO_MAP, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );
        assert_err(
            call(&mut k, num::EVENT_SIGNAL, &[event, 1, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );
        assert_err(
            call(&mut k, num::RECV, &[ep, 0, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_handle_dup_with_zero_rights() {
        let mut k = setup_kernel();
        let h = create_vmo(&mut k);
        let dup = assert_ok(call(
            &mut k,
            num::HANDLE_DUP,
            &[h, Rights::NONE.0 as u64, 0, 0, 0, 0],
        ));

        assert_err(
            call(&mut k, num::VMO_MAP, &[dup, 0, 0, 0, 0, 0]),
            SyscallError::InsufficientRights,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_map_then_unmap_then_remap() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);
        let va = assert_ok(call(
            &mut k,
            num::VMO_MAP,
            &[vmo, 0, Rights::READ.0 as u64, 0, 0, 0],
        ));

        assert_ok(call(&mut k, num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]));

        let va2 = assert_ok(call(
            &mut k,
            num::VMO_MAP,
            &[vmo, 0, Rights::READ.0 as u64, 0, 0, 0],
        ));

        assert_eq!(va, va2);

        inv(&k);
    }

    #[test]
    fn adversarial_snapshot_sealed_vmo() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]));

        let snap = assert_ok(call(&mut k, num::VMO_SNAPSHOT, &[vmo, 0, 0, 0, 0, 0]));

        assert_ok(call(
            &mut k,
            num::VMO_RESIZE,
            &[snap, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
        ));

        inv(&k);
    }

    #[test]
    fn adversarial_endpoint_bind_event_then_close_event() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let event = create_event(&mut k);

        assert_ok(call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep, event, 0, 0, 0, 0],
        ));
        assert_ok(call(&mut k, num::HANDLE_CLOSE, &[event, 0, 0, 0, 0, 0]));

        inv(&k);
    }

    #[test]
    fn adversarial_thread_exit_then_space_destroy() {
        let mut k = setup_kernel();
        let space = create_space(&mut k);

        assert_ok(call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space, 0x1000, 0x2000, 0, 0, 0],
        ));
        assert_ok(call(&mut k, num::SPACE_DESTROY, &[space, 0, 0, 0, 0, 0]));

        inv(&k);
    }

    #[test]
    fn adversarial_event_wait_with_destroyed_event_handle() {
        let mut k = setup_kernel();
        let event = create_event(&mut k);
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(event as u32))
            .unwrap()
            .object_id;

        k.events.get_mut(obj_id).unwrap().signal(0xFF);

        let r = call(&mut k, num::EVENT_WAIT, &[event, 0xFF, 0, 0, 0, 0]);

        assert_ok(r);

        inv(&k);
    }

    #[test]
    fn adversarial_vmo_map_into_cross_space_then_destroy_target() {
        let mut k = setup_kernel();
        let target_space = create_space(&mut k);
        let vmo = create_vmo(&mut k);

        assert_ok(call(
            &mut k,
            num::VMO_MAP_INTO,
            &[vmo, target_space, 0, Rights::READ.0 as u64, 0, 0],
        ));
        assert_ok(call(
            &mut k,
            num::SPACE_DESTROY,
            &[target_space, 0, 0, 0, 0, 0],
        ));

        inv(&k);
    }

    #[test]
    fn adversarial_rapid_create_destroy_all_types_100x() {
        let mut k = setup_kernel();

        for _ in 0..100 {
            let v = create_vmo(&mut k);
            let e = create_event(&mut k);
            let ep = create_endpoint(&mut k);

            call(&mut k, num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);
            call(&mut k, num::HANDLE_CLOSE, &[e, 0, 0, 0, 0, 0]);
            call(&mut k, num::HANDLE_CLOSE, &[v, 0, 0, 0, 0, 0]);
        }

        inv(&k);
    }

    #[test]
    fn adversarial_thread_set_priority_all_values() {
        let mut k = setup_kernel();
        let t = create_thread(&mut k);

        for pri in 0..=3u64 {
            assert_ok(call(
                &mut k,
                num::THREAD_SET_PRIORITY,
                &[t, pri, 0, 0, 0, 0],
            ));
        }

        assert_err(
            call(&mut k, num::THREAD_SET_PRIORITY, &[t, 4, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );
        assert_err(
            call(&mut k, num::THREAD_SET_PRIORITY, &[t, 255, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_thread_set_affinity_all_values() {
        let mut k = setup_kernel();
        let t = create_thread(&mut k);

        for hint in 0..=2u64 {
            assert_ok(call(
                &mut k,
                num::THREAD_SET_AFFINITY,
                &[t, hint, 0, 0, 0, 0],
            ));
        }

        assert_err(
            call(&mut k, num::THREAD_SET_AFFINITY, &[t, 3, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_system_info_all_selectors() {
        let mut k = setup_kernel();

        assert_ok(call(&mut k, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]));
        assert_ok(call(&mut k, num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]));
        assert_ok(call(&mut k, num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::SYSTEM_INFO, &[3, 0, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );
        assert_err(
            call(&mut k, num::SYSTEM_INFO, &[u64::MAX, 0, 0, 0, 0, 0]),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn adversarial_seal_then_all_mutating_ops() {
        let mut k = setup_kernel();
        let vmo = create_vmo(&mut k);

        assert_ok(call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]));
        assert_err(
            call(&mut k, num::VMO_SEAL, &[vmo, 0, 0, 0, 0, 0]),
            SyscallError::AlreadySealed,
        );
        assert_err(
            call(
                &mut k,
                num::VMO_RESIZE,
                &[vmo, config::PAGE_SIZE as u64 * 2, 0, 0, 0, 0],
            ),
            SyscallError::AlreadySealed,
        );

        inv(&k);
    }

    // ── Convergence pass 1: boundary value tests ──

    #[test]
    fn boundary_handle_table_fill_and_recover() {
        let mut k = setup_kernel();
        let mut handles = alloc::vec::Vec::new();
        // Fill handle table to capacity (minus the 2 bootstrap handles: space + thread 0).
        let initial = k.spaces.get(0).unwrap().handles().count();

        for _ in initial..config::MAX_HANDLES {
            handles.push(create_vmo(&mut k));
        }

        assert_err(
            call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]),
            SyscallError::OutOfMemory,
        );

        call(&mut k, num::HANDLE_CLOSE, &[handles[0], 0, 0, 0, 0, 0]);
        create_vmo(&mut k);

        inv(&k);
    }

    #[test]
    fn boundary_event_wait_max_multi_wait_inline() {
        let mut k = setup_kernel();
        let e1 = create_event(&mut k);
        let e2 = create_event(&mut k);
        let e3 = create_event(&mut k);
        let obj_id2 = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(e2 as u32))
            .unwrap()
            .object_id;

        k.events.get_mut(obj_id2).unwrap().signal(0b1);

        let r = call(&mut k, num::EVENT_WAIT, &[e1, 0b1, e2, 0b1, e3, 0b1]);

        assert_eq!(r.0, 0);
        assert_eq!(r.1, e2);

        inv(&k);
    }

    #[test]
    fn boundary_vmo_create_exactly_one_page() {
        let mut k = setup_kernel();
        let h = assert_ok(call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        ));
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(h as u32))
            .unwrap()
            .object_id;

        assert_eq!(k.vmos.get(obj_id).unwrap().page_count(), 1);

        inv(&k);
    }

    #[test]
    fn boundary_vmo_create_one_byte() {
        let mut k = setup_kernel();
        let h = assert_ok(call(&mut k, num::VMO_CREATE, &[1, 0, 0, 0, 0, 0]));
        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(h as u32))
            .unwrap()
            .object_id;

        assert_eq!(k.vmos.get(obj_id).unwrap().page_count(), 1);
        assert_eq!(k.vmos.get(obj_id).unwrap().size(), 1);

        inv(&k);
    }

    #[test]
    fn boundary_ipc_max_handles_transfer() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let mut vmos = [0u64; config::MAX_IPC_HANDLES];

        for v in &mut vmos {
            *v = create_vmo(&mut k);
        }

        let handle_ids: alloc::vec::Vec<u32> = vmos.iter().map(|h| *h as u32).collect();
        let ptr = handle_ids.as_ptr() as usize as u64;
        let r = call(
            &mut k,
            num::CALL,
            &[ep, 0, 0, ptr, config::MAX_IPC_HANDLES as u64, 0],
        );

        assert_eq!(r.0, 0);

        inv(&k);
    }

    #[test]
    fn boundary_ipc_too_many_handles() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        assert_err(
            call(
                &mut k,
                num::CALL,
                &[ep, 0, 0, 0, config::MAX_IPC_HANDLES as u64 + 1, 0],
            ),
            SyscallError::InvalidArgument,
        );

        inv(&k);
    }

    #[test]
    fn boundary_clock_read_returns_nonzero() {
        let mut k = setup_kernel();
        let r = call(&mut k, num::CLOCK_READ, &[0; 6]);

        assert_eq!(r.0, 0);

        inv(&k);
    }

    #[test]
    fn boundary_event_signal_bit_63() {
        let mut k = setup_kernel();
        let e = create_event(&mut k);

        assert_ok(call(
            &mut k,
            num::EVENT_SIGNAL,
            &[e, 1u64 << 63, 0, 0, 0, 0],
        ));

        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(e as u32))
            .unwrap()
            .object_id;

        assert_eq!(k.events.get(obj_id).unwrap().bits(), 1u64 << 63);

        inv(&k);
    }

    #[test]
    fn boundary_event_clear_all_bits() {
        let mut k = setup_kernel();
        let e = create_event(&mut k);

        assert_ok(call(&mut k, num::EVENT_SIGNAL, &[e, u64::MAX, 0, 0, 0, 0]));
        assert_ok(call(&mut k, num::EVENT_CLEAR, &[e, u64::MAX, 0, 0, 0, 0]));

        let obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(e as u32))
            .unwrap()
            .object_id;

        assert_eq!(k.events.get(obj_id).unwrap().bits(), 0);

        inv(&k);
    }

    // ── Regression tests for pass-3 critical findings ──

    #[test]
    fn regression_thread_create_rollback_does_not_leave_scheduler_dangling() {
        let mut k = setup_kernel();
        // Fill the handle table so the next thread_create will fail at handle allocation.
        let mut handles = alloc::vec::Vec::new();

        for _ in k.spaces.get(0).unwrap().handles().count()..config::MAX_HANDLES {
            handles.push(create_vmo(&mut k));
        }

        // Now thread_create should fail because handle table is full.
        assert_err(
            call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]),
            SyscallError::OutOfMemory,
        );

        // The scheduler must NOT have a dangling thread ID.
        assert_eq!(k.scheduler.core(0).total_ready(), 0);

        inv(&k);
    }

    #[test]
    fn regression_thread_create_in_rollback_does_not_leave_scheduler_dangling() {
        let mut k = setup_kernel();
        let space = create_space(&mut k);
        // Fill handle table.
        let mut handles = alloc::vec::Vec::new();

        for _ in k.spaces.get(0).unwrap().handles().count()..config::MAX_HANDLES {
            handles.push(create_vmo(&mut k));
        }

        assert_err(
            call(
                &mut k,
                num::THREAD_CREATE_IN,
                &[space, 0x1000, 0x2000, 0, 0, 0],
            ),
            SyscallError::OutOfMemory,
        );

        // No dangling threads in any run queue.
        for core_id in 0..k.scheduler.num_cores() {
            assert_eq!(k.scheduler.core(core_id).total_ready(), 0);
        }

        inv(&k);
    }

    // =========================================================================
    // BUG REGRESSION TESTS — each test exercises a bug found by Phase 0
    // adversarial spec review. If any of these fail, the fix has regressed.
    // =========================================================================

    #[test]
    fn regression_call_on_peer_closed_preserves_handles() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);
        let vmo = create_vmo(&mut k);

        let dup = assert_ok(call(
            &mut k,
            num::HANDLE_DUP,
            &[vmo, Rights::ALL.0 as u64, 0, 0, 0, 0],
        ));

        let initial_handle_count = k.spaces.get(0).unwrap().handles().count();

        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep as u32))
            .unwrap()
            .object_id;

        k.endpoints.get_mut(ep_obj_id).unwrap().close_peer();

        let handle_ids = [dup as u32];
        let handles_ptr = handle_ids.as_ptr() as usize as u64;
        let result = call(&mut k, num::CALL, &[ep, 0, 0, handles_ptr, 1, 0]);

        assert_eq!(result.0, SyscallError::PeerClosed as u64);

        let after_handle_count = k.spaces.get(0).unwrap().handles().count();

        assert_eq!(
            initial_handle_count, after_handle_count,
            "handles must not be leaked when call fails on peer-closed endpoint"
        );

        inv(&k);
    }

    #[test]
    fn regression_space_destroy_decrements_alive_threads() {
        let mut k = setup_kernel();

        let space = assert_ok(call(&mut k, num::SPACE_CREATE, &[0; 6]));
        let thread = assert_ok(call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space, 0x1000, 0x2000, 0, 0, 0],
        ));
        let _ = thread;

        let before = k.alive_threads;

        assert_ok(call(&mut k, num::SPACE_DESTROY, &[space, 0, 0, 0, 0, 0]));

        assert_eq!(
            k.alive_threads,
            before - 1,
            "space_destroy must decrement alive_threads for killed threads"
        );

        inv(&k);
    }

    #[test]
    fn regression_event_wait_waiter_leak_on_partial_failure() {
        let mut k = setup_kernel();

        let evt0 = create_event(&mut k);
        let evt1 = create_event(&mut k);

        let evt0_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt0 as u32))
            .unwrap()
            .object_id;
        let evt1_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt1 as u32))
            .unwrap()
            .object_id;

        let event = k.events.get_mut(evt1_obj_id).unwrap();

        for i in 0..config::MAX_WAITERS_PER_EVENT {
            event.add_waiter(ThreadId(1000 + i as u32), 0b1).unwrap();
        }

        let result = call(&mut k, num::EVENT_WAIT, &[evt0, 0b1, evt1, 0b1, 0, 0]);

        assert_eq!(result.0, SyscallError::BufferFull as u64);

        let evt0_waiters = k.events.get(evt0_obj_id).unwrap().waiter_count();

        assert_eq!(
            evt0_waiters, 0,
            "event 0 should have no leftover waiters after partial multi-wait failure"
        );
    }

    // -- Endpoint destruction during blocked call (Phase 0 bug fixes) --

    #[test]
    fn caller_gets_peer_closed_on_endpoint_destroy() {
        let mut k = setup_kernel();

        k.threads.get_mut(0).unwrap().init_register_state();

        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let ep_handle_ids = [ep_hid as u32];
        let (_, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[
                space_hid,
                0x1000,
                0x2000,
                0,
                ep_handle_ids.as_ptr() as u64,
                1,
            ],
        );

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 4, 0, 0, 0],
        );

        assert_eq!(err, 0);
        assert!(
            k.endpoints.get(ep_obj_id).unwrap().pending_call_count() > 0,
            "call should be pending in the endpoint"
        );

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let thread = k.threads.get_mut(0).unwrap();

        assert_eq!(
            thread.take_wakeup_error(),
            Some(SyscallError::PeerClosed),
            "blocked caller must get PeerClosed when endpoint is destroyed"
        );

        #[cfg(any(target_os = "none", test))]
        {
            let rs = k.threads.get(0).unwrap().register_state().unwrap();

            assert_eq!(
                rs.gprs[0],
                SyscallError::PeerClosed as u64,
                "register state x0 must reflect PeerClosed error"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handles_recovered_when_endpoint_destroyed_during_call() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let ep_handle_ids = [ep_hid as u32];
        let (_, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[
                space_hid,
                0x1000,
                0x2000,
                0,
                ep_handle_ids.as_ptr() as u64,
                1,
            ],
        );

        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        let (err, _) = k.dispatch(ThreadId(0), 0, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0, "VMO handle should be valid before call");

        let handle_ids = [vmo_hid as u32];
        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                buf.as_mut_ptr() as u64,
                0,
                handle_ids.as_ptr() as u64,
                1,
                0,
            ],
        );

        assert_eq!(err, 0);

        let (err, _) = k.dispatch(ThreadId(0), 0, num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(
            err,
            SyscallError::InvalidHandle as u64,
            "handle should be removed during call"
        );

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let space = k.spaces.get(0).unwrap();
        let mut found_vmo = false;

        for (_, h) in space.handles().iter_handles() {
            if h.object_type == ObjectType::Vmo {
                found_vmo = true;

                break;
            }
        }

        assert!(
            found_vmo,
            "transferred VMO handle must be reinstalled after endpoint destruction"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn recv_waiter_gets_peer_closed_on_endpoint_destroy() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let ep_handle_ids = [ep_hid as u32];
        let (_, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[
                space_hid,
                0x1000,
                0x2000,
                0,
                ep_handle_ids.as_ptr() as u64,
                1,
            ],
        );

        let mut recv_buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_ne!(err, 0, "recv with no pending calls should not succeed");

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let thread = k.threads.get_mut(0).unwrap();

        assert_eq!(
            thread.take_wakeup_error(),
            Some(SyscallError::PeerClosed),
            "recv waiter must get PeerClosed when endpoint is destroyed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_frees_endpoint() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        assert!(k.endpoints.get(ep_obj_id).is_some());

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[ep_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(
            k.endpoints.get(ep_obj_id).is_none(),
            "endpoint must be freed when last handle is closed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_frees_event() {
        let mut k = setup_kernel();
        let (_, evt_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let evt_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt_hid as u32))
            .unwrap()
            .object_id;

        assert!(k.events.get(evt_obj_id).is_some());

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[evt_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(
            k.events.get(evt_obj_id).is_none(),
            "event must be freed when last handle is closed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_frees_vmo() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let vmo_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_hid as u32))
            .unwrap()
            .object_id;

        assert!(k.vmos.get(vmo_obj_id).is_some());

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[vmo_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(
            k.vmos.get(vmo_obj_id).is_none(),
            "VMO must be freed when last handle is closed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_dup_prevents_premature_free() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;
        let (_, dup_hid) = call(
            &mut k,
            num::HANDLE_DUP,
            &[ep_hid, Rights::ALL.0 as u64, 0, 0, 0, 0],
        );

        assert_eq!(k.endpoints.get(ep_obj_id).unwrap().refcount(), 2);

        call(&mut k, num::HANDLE_CLOSE, &[ep_hid, 0, 0, 0, 0, 0]);

        assert!(
            k.endpoints.get(ep_obj_id).is_some(),
            "endpoint must survive when other handles still reference it"
        );
        assert_eq!(k.endpoints.get(ep_obj_id).unwrap().refcount(), 1);

        call(&mut k, num::HANDLE_CLOSE, &[dup_hid, 0, 0, 0, 0, 0]);

        assert!(
            k.endpoints.get(ep_obj_id).is_none(),
            "endpoint must be freed when last handle is closed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_buffer_path() {
        let mut k = setup_kernel();
        let (_, evt0) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, evt1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[evt0, 0b10, 0, 0, 0, 0]);

        let wait_buf: [u32; 6] = [evt0 as u32, 0b10, 0, evt1 as u32, 0b01, 0];
        let result = call(
            &mut k,
            num::EVENT_WAIT,
            &[wait_buf.as_ptr() as u64, 2, 0, 0, 0, 0],
        );

        assert_eq!(result.0, 0);
        assert_eq!(result.1, evt0);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_endpoint_with_bound_event() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, evt_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;
        let evt_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt_hid as u32))
            .unwrap()
            .object_id;

        let (err, _) = call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, evt_hid, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);
        assert!(k.endpoints.get(ep_obj_id).unwrap().bound_event().is_some());

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[ep_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(
            k.endpoints.get(ep_obj_id).is_none(),
            "endpoint should be freed"
        );
        assert!(
            k.events.get(evt_obj_id).unwrap().bound_endpoint().is_none(),
            "event's bound_endpoint should be cleared when endpoint is destroyed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_event_with_bound_endpoint() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, evt_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;
        let evt_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt_hid as u32))
            .unwrap()
            .object_id;

        call(
            &mut k,
            num::ENDPOINT_BIND_EVENT,
            &[ep_hid, evt_hid, 0, 0, 0, 0],
        );

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[evt_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(k.events.get(evt_obj_id).is_none(), "event should be freed");
        assert!(
            k.endpoints.get(ep_obj_id).unwrap().bound_event().is_none(),
            "endpoint's bound_event should be cleared when event is destroyed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn handle_close_event_with_irq_binding() {
        let mut k = setup_kernel();
        let (_, evt_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let evt_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(evt_hid as u32))
            .unwrap()
            .object_id;

        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[evt_hid, 32, 0b1, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(k.irqs.binding_at(32).is_some());

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[evt_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(k.events.get(evt_obj_id).is_none());
        assert!(
            k.irqs.binding_at(32).is_none(),
            "IRQ binding must be removed when event is destroyed"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn vmo_resize_blocked_by_active_mapping() {
        let mut k = setup_kernel();
        let page = config::PAGE_SIZE as u64;
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[page * 4, 0, 0, 0, 0, 0]);
        let (err, _) = call(
            &mut k,
            num::VMO_MAP,
            &[vmo_hid, 0, Rights::READ.0 as u64, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::VMO_RESIZE, &[vmo_hid, page, 0, 0, 0, 0]);

        assert_eq!(
            err,
            SyscallError::InvalidArgument as u64,
            "resize below active mapping size must fail"
        );

        crate::invariants::assert_valid(&*k);
    }

    // -- Error injection: capacity exhaustion --

    #[test]
    fn handle_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut last_good = 0u64;

        for i in 0..config::MAX_HANDLES {
            let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                assert_eq!(
                    err,
                    SyscallError::OutOfMemory as u64,
                    "expected OutOfMemory at handle {i}"
                );

                break;
            }

            last_good = hid;
        }

        assert!(last_good > 0, "should have created at least one event");

        call(&mut k, num::HANDLE_CLOSE, &[last_good, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0, "should succeed after freeing a slot");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn endpoint_table_exhaustion() {
        let mut k = setup_kernel();
        let mut created = 0;

        for _ in 0..config::MAX_ENDPOINTS + 1 {
            let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

            if err != 0 {
                assert_eq!(err, SyscallError::OutOfMemory as u64);

                break;
            }

            created += 1;
        }

        assert!(
            created <= config::MAX_ENDPOINTS,
            "should not exceed MAX_ENDPOINTS"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_rollback_on_handle_table_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES - 1 {
            let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let initial_thread_count = k.threads.count();
        let (err, _) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        if err != 0 {
            assert_eq!(
                k.threads.count(),
                initial_thread_count,
                "thread table should not grow on failed create"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn error_inject_vmo_snapshot_rollback_handles_full() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );

        for _ in 0..config::MAX_HANDLES - 2 {
            let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let initial_vmo_count = k.vmos.count();
        let (err, _) = call(&mut k, num::VMO_SNAPSHOT, &[vmo_hid, 0, 0, 0, 0, 0]);

        if err != 0 {
            assert_eq!(
                k.vmos.count(),
                initial_vmo_count,
                "VMO table should not grow on failed snapshot"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn error_inject_space_create_rollback_handles_full() {
        let mut k = setup_kernel();

        for _ in 0..config::MAX_HANDLES - 1 {
            let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let initial_space_count = k.spaces.count();
        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        if err != 0 {
            assert_eq!(
                k.spaces.count(),
                initial_space_count,
                "space table should not grow on failed create"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    // -- Error code audit: untested error paths --

    #[test]
    fn call_on_full_endpoint_returns_buffer_full() {
        use crate::{
            endpoint::{Message, PendingCall},
            thread::Thread,
        };

        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        let priorities = [
            Priority::Idle,
            Priority::Low,
            Priority::Medium,
            Priority::High,
        ];

        for i in 0..config::MAX_PENDING_PER_ENDPOINT {
            let t = Thread::new(
                ThreadId(0),
                Some(AddressSpaceId(0)),
                priorities[i % 4],
                0,
                0,
                0,
            );
            let (tid, _) = k.threads.alloc(t).unwrap();

            k.threads.get_mut(tid).unwrap().id = ThreadId(tid);
            k.threads
                .get_mut(tid)
                .unwrap()
                .set_state(crate::thread::ThreadRunState::Blocked);

            let ep = k.endpoints.get_mut(ep_obj_id).unwrap();
            let pending = PendingCall {
                caller: ThreadId(tid),
                priority: priorities[i % 4],
                message: Message::empty(),
                handles: [const { None }; config::MAX_IPC_HANDLES],
                handle_count: 0,
                badge: 0,
                reply_buf: 0,
            };

            ep.enqueue_call(pending).unwrap();
        }

        assert!(k.endpoints.get(ep_obj_id).unwrap().is_full());

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(
            err,
            SyscallError::BufferFull as u64,
            "call on full endpoint must return BufferFull"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn call_on_peer_closed_endpoint() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        k.endpoints.get_mut(ep_obj_id).unwrap().close_peer();

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[ep_hid, buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::PeerClosed as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_buffer_wrong_handle_type() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );
        let wait_buf: [u32; 3] = [vmo_hid as u32, 0b1, 0];
        let result = call(
            &mut k,
            num::EVENT_WAIT,
            &[wait_buf.as_ptr() as u64, 1, 0, 0, 0, 0],
        );

        assert_eq!(result.0, SyscallError::WrongHandleType as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_buffer_zero_count() {
        let mut k = setup_kernel();
        let buf: [u32; 3] = [0, 0, 0];
        let result = call(
            &mut k,
            num::EVENT_WAIT,
            &[buf.as_ptr() as u64, 0, 0, 0, 0, 0],
        );

        assert_eq!(result.0, SyscallError::InvalidArgument as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_wait_buffer_all_zero_masks() {
        let mut k = setup_kernel();
        let (_, evt_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let wait_buf: [u32; 3] = [evt_hid as u32, 0, 0];
        let result = call(
            &mut k,
            num::EVENT_WAIT,
            &[wait_buf.as_ptr() as u64, 1, 0, 0, 0, 0],
        );

        assert_eq!(
            result.0,
            SyscallError::InvalidArgument as u64,
            "all-zero masks means no events to wait on"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn reply_to_non_blocked_caller() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let mut call_buf = [0u8; 128];

        call(
            &mut k,
            num::CALL,
            &[ep_hid, call_buf.as_mut_ptr() as u64, 4, 0, 0, 0],
        );

        let mut recv_buf = [0u8; 128];
        let (_, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        let reply_cap = packed >> 32;

        crate::sched::wake(&mut k, ThreadId(0), 0);

        assert_eq!(
            k.threads.get(0).unwrap().state(),
            crate::thread::ThreadRunState::Ready,
        );

        let (err, _) = call(&mut k, num::REPLY, &[ep_hid, reply_cap, 0, 0, 0, 0]);

        assert_eq!(
            err,
            SyscallError::InvalidArgument as u64,
            "reply to non-blocked caller must fail"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn reply_with_handles_exceeds_caller_capacity() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let mut call_buf = [0u8; 128];

        call(
            &mut k,
            num::CALL,
            &[ep_hid, call_buf.as_mut_ptr() as u64, 0, 0, 0, 0],
        );

        let mut recv_buf = [0u8; 128];
        let (_, packed) = call(
            &mut k,
            num::RECV,
            &[ep_hid, recv_buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        let reply_cap = packed >> 32;

        for _ in 0..config::MAX_HANDLES - 2 {
            let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }
        }

        let (_, extra) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        if extra > 0 {
            let handle_ids = [extra as u32];
            let (err, _) = call(
                &mut k,
                num::REPLY,
                &[ep_hid, reply_cap, 0, 0, handle_ids.as_ptr() as u64, 1],
            );

            assert_eq!(
                err,
                SyscallError::BufferFull as u64,
                "reply with handles when caller's handle table is full"
            );
        }

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn recv_on_peer_closed_endpoint_returns_error() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let ep_obj_id = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        k.endpoints.get_mut(ep_obj_id).unwrap().close_peer();

        let mut buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::RECV,
            &[ep_hid, buf.as_mut_ptr() as u64, 128, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::PeerClosed as u64);

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn install_handles_buffer_full() {
        let mut k = setup_kernel();
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );

        let handle_ids = [vmo_hid as u32];
        let mut call_buf = [0u8; 128];
        let (err, _) = call(
            &mut k,
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                0,
                handle_ids.as_ptr() as u64,
                1,
                0,
            ],
        );

        assert_eq!(err, 0);

        let mut recv_buf = [0u8; 128];
        let mut recv_handles = [0u32; 1];
        let (err, _) = call(
            &mut k,
            num::RECV,
            &[
                ep_hid,
                recv_buf.as_mut_ptr() as u64,
                128,
                recv_handles.as_mut_ptr() as u64,
                0,
                0,
            ],
        );

        assert_eq!(
            err,
            SyscallError::BufferFull as u64,
            "recv with handle_cap=0 when call transferred handles must fail"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_with_handles_increments_refcounts() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let (_, vmo_hid) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        let vmo_obj = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(vmo_hid as u32))
            .unwrap()
            .object_id;
        let ep_obj = k
            .spaces
            .get(0)
            .unwrap()
            .handles()
            .lookup(HandleId(ep_hid as u32))
            .unwrap()
            .object_id;

        assert_eq!(k.vmos.get(vmo_obj).unwrap().refcount(), 1);
        assert_eq!(k.endpoints.get(ep_obj).unwrap().refcount(), 1);

        let handle_ids = [vmo_hid as u32, ep_hid as u32];
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[space_hid, 0x1000, 0x2000, 0, handle_ids.as_ptr() as u64, 2],
        );

        assert_eq!(err, 0);
        assert_eq!(
            k.vmos.get(vmo_obj).unwrap().refcount(),
            2,
            "VMO refcount should be 2 after cloning handle into child space"
        );
        assert_eq!(
            k.endpoints.get(ep_obj).unwrap().refcount(),
            2,
            "endpoint refcount should be 2 after cloning handle into child space"
        );

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(
            k.vmos.get(vmo_obj).unwrap().refcount(),
            1,
            "VMO refcount should return to 1 after space destroy"
        );
        assert_eq!(
            k.endpoints.get(ep_obj).unwrap().refcount(),
            1,
            "endpoint refcount should return to 1 after space destroy"
        );

        crate::invariants::assert_valid(&*k);
    }

    // -- Error injection: object table exhaustion --

    #[test]
    fn vmo_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut created = 0;
        let mut last_hid = 0u64;

        loop {
            let (err, hid) = call(
                &mut k,
                num::VMO_CREATE,
                &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
            );

            if err != 0 {
                assert!(
                    err == SyscallError::OutOfMemory as u64,
                    "expected OutOfMemory, got {err}"
                );

                break;
            }

            last_hid = hid;
            created += 1;
        }

        assert!(created > 0);

        call(&mut k, num::HANDLE_CLOSE, &[last_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(
            &mut k,
            num::VMO_CREATE,
            &[config::PAGE_SIZE as u64, 0, 0, 0, 0, 0],
        );

        assert_eq!(err, 0, "should recover after freeing one VMO");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn event_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut created = 0;
        let mut last_hid = 0u64;

        loop {
            let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

            if err != 0 {
                assert!(
                    err == SyscallError::OutOfMemory as u64,
                    "expected OutOfMemory, got {err}"
                );

                break;
            }

            last_hid = hid;
            created += 1;
        }

        assert!(created > 0);

        call(&mut k, num::HANDLE_CLOSE, &[last_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0, "should recover after freeing one event");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn thread_create_in_invalid_handle_rollback() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);
        let initial_thread_count = k.threads.count();

        let bad_handle_ids = [9999u32];
        let (err, _) = call(
            &mut k,
            num::THREAD_CREATE_IN,
            &[
                space_hid,
                0x1000,
                0x2000,
                0,
                bad_handle_ids.as_ptr() as u64,
                1,
            ],
        );

        assert_ne!(err, 0, "should fail with invalid handle");
        assert_eq!(
            k.threads.count(),
            initial_thread_count,
            "thread should be rolled back"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn space_table_exhaustion_and_recovery() {
        let mut k = setup_kernel();
        let mut created = 0;
        let mut last_hid = 0u64;

        loop {
            let (err, hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

            if err != 0 {
                break;
            }

            last_hid = hid;
            created += 1;
        }

        assert!(created > 0);

        call(&mut k, num::SPACE_DESTROY, &[last_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0, "should recover after destroying one space");

        crate::invariants::assert_valid(&*k);
    }

    // -- Phase 8: Multi-core scheduling stress --

    #[test]
    fn multi_core_thread_creation_and_scheduling() {
        crate::frame::arch::page_table::reset_asid_pool();

        let mut k = Box::new(Kernel::new(2));
        let space = AddressSpace::new(AddressSpaceId(0), 1, 0);

        k.spaces.alloc(space);

        let t0 = Thread::new(
            ThreadId(0),
            Some(AddressSpaceId(0)),
            Priority::Medium,
            0,
            0,
            0,
        );

        k.threads.alloc(t0);
        k.threads
            .get_mut(0)
            .unwrap()
            .set_state(ThreadRunState::Running);
        k.scheduler.core_mut(0).set_current(Some(ThreadId(0)));

        let (err, _) = k.dispatch(
            ThreadId(0),
            0,
            num::THREAD_CREATE,
            &[0x1000, 0x2000, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let (err, _) = k.dispatch(
            ThreadId(0),
            0,
            num::THREAD_CREATE,
            &[0x3000, 0x4000, 0, 0, 0, 0],
        );

        assert_eq!(err, 0);

        let total_ready = k.scheduler.core(0).total_ready() + k.scheduler.core(1).total_ready();

        assert!(total_ready >= 2, "both new threads should be ready");

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn multi_core_event_signal_cross_core_wake() {
        let mut k = setup_kernel();
        let evt = create_event(&mut k);

        k.dispatch(ThreadId(0), 0, num::EVENT_WAIT, &[evt, 0b1, 0, 0, 0, 0]);

        assert_eq!(
            k.threads.get(0).unwrap().state(),
            ThreadRunState::Blocked,
            "thread 0 should be blocked waiting for event"
        );

        k.events.get_mut(0).unwrap().signal(0b1);

        crate::sched::wake(&mut k, ThreadId(0), 0);

        assert_eq!(
            k.threads.get(0).unwrap().state(),
            ThreadRunState::Ready,
            "thread 0 should be woken after signal"
        );

        crate::invariants::assert_valid(&*k);
    }

    #[test]
    fn rapid_create_destroy_cycle_stress() {
        let mut k = setup_kernel();

        for _ in 0..100 {
            let vmo = create_vmo(&mut k);
            let evt = create_event(&mut k);
            let ep = create_endpoint(&mut k);

            call(&mut k, num::HANDLE_CLOSE, &[vmo, 0, 0, 0, 0, 0]);
            call(&mut k, num::HANDLE_CLOSE, &[evt, 0, 0, 0, 0, 0]);
            call(&mut k, num::HANDLE_CLOSE, &[ep, 0, 0, 0, 0, 0]);
        }

        crate::invariants::assert_valid(&*k);

        assert_eq!(k.vmos.count(), 0, "all VMOs should be freed");
        assert_eq!(k.events.count(), 0, "all events should be freed");
        assert_eq!(k.endpoints.count(), 0, "all endpoints should be freed");
    }

    #[test]
    fn ipc_call_recv_reply_50_rapid_rounds() {
        let mut k = setup_kernel();
        let ep = create_endpoint(&mut k);

        for round in 0..50u8 {
            let msg = [round];
            let mut reply_buf = [0u8; 128];

            do_call(&mut k, ep, &msg, &mut reply_buf);

            let mut recv_buf = [0u8; 128];
            let (len, reply_cap) = do_recv(&mut k, ep, &mut recv_buf);

            assert_eq!(&recv_buf[..len], &[round]);

            do_reply(&mut k, ep, reply_cap, &[round + 100]);
            resume_caller(&mut k);
        }

        crate::invariants::assert_valid(&*k);
    }
}
