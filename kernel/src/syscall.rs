//! Syscall dispatch — maps syscall numbers to kernel object operations.
//!
//! The Kernel struct owns all object tables and the scheduler. Each syscall
//! handler extracts arguments, validates handles/rights, calls the kernel
//! object method, and returns (error_code, return_value).

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
    pub irqs: IrqTable,
    pub scheduler: Scheduler,
}

impl Kernel {
    pub fn new(num_cores: usize) -> Self {
        Kernel {
            vmos: ObjectTable::new(),
            events: ObjectTable::new(),
            endpoints: ObjectTable::new(),
            threads: ObjectTable::new(),
            spaces: ObjectTable::new(),
            irqs: IrqTable::new(),
            scheduler: Scheduler::new(num_cores),
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

    fn link_thread_to_space(&mut self, thread_idx: u32, space_id: AddressSpaceId) {
        let old_head = self
            .spaces
            .get(space_id.0)
            .and_then(|s| s.thread_head());

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
        let prev = self
            .threads
            .get(thread_idx)
            .and_then(|t| t.space_prev());
        let next = self
            .threads
            .get(thread_idx)
            .and_then(|t| t.space_next());

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

        Ok(space.handles().lookup(handle_id)?.clone())
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
    pub fn dispatch(&mut self, current: ThreadId, syscall_num: u64, args: &[u64; 6]) -> (u64, u64) {
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

        let _freed = self
            .vmos
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .resize(new_size)?;

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
            crate::sched::wake(self, info.thread_id, 0);
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
        let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
        let (idx, generation) = self.spaces.alloc(space).ok_or(SyscallError::OutOfMemory)?;

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

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let new_id = space.handles_mut().duplicate(handle_id, new_rights)?;

        Ok(new_id.0 as u64)
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

        space.handles_mut().close(handle_id)?;

        Ok(0)
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

        for &(hid, obj_id, mask) in &wait_items[..count] {
            let event = self
                .events
                .get_mut(obj_id)
                .ok_or(SyscallError::InvalidHandle)?;

            if let Some(fired) = event.check(mask) {
                return Ok(((hid as u64) << 32) | (fired & 0xFFFF_FFFF));
            }
        }

        let mut obj_ids = [0u32; 3];
        for (i, &(_, obj_id, mask)) in wait_items[..count].iter().enumerate() {
            let event = self
                .events
                .get_mut(obj_id)
                .ok_or(SyscallError::InvalidHandle)?;

            event.add_waiter(current, mask)?;

            obj_ids[i] = obj_id;
        }

        self.threads
            .get_mut(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .set_wait_events(&obj_ids[..count]);

        crate::sched::block_current(self, current, 0);

        let (wait_evts, wait_n) = self
            .threads
            .get_mut(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .take_wait_events();

        for i in 0..wait_n as usize {
            let obj_id = wait_evts[i];
            let (hid, _, mask) = wait_items
                .iter()
                .find(|&&(_, oid, _)| oid == obj_id)
                .copied()
                .unwrap();
            let event = self
                .events
                .get_mut(obj_id)
                .ok_or(SyscallError::InvalidHandle)?;

            if let Some(fired) = event.check(mask) {
                for (j, &evt_id) in wait_evts[..wait_n as usize].iter().enumerate() {
                    if j != i {
                        self.events
                            .get_mut(evt_id)
                            .map(|e| e.remove_waiter(current));
                    }
                }

                return Ok(((hid as u64) << 32) | (fired & 0xFFFF_FFFF));
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
        let signal_info = ep.enqueue_call(call)?;
        let recv_waiters = ep.drain_recv_waiters();

        if let Some((event_id, bits)) = signal_info
            && let Some(event) = self.events.get_mut(event_id.0)
        {
            let woken = event.signal(bits);

            for info in woken.as_slice() {
                crate::sched::wake(self, info.thread_id, 0);
            }
        }

        for waiter in recv_waiters.as_slice() {
            crate::sched::wake(self, *waiter, 0);
        }

        crate::sched::block_current(self, current, 0);

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
        crate::sched::block_current(self, current, 0);

        if let Some(result) = self.try_dequeue_and_deliver(
            obj_id,
            space_id,
            out_buf,
            out_cap,
            handles_out,
            handles_cap,
        ) {
            return result;
        }

        Err(SyscallError::PeerClosed)
    }

    fn try_dequeue_and_deliver(
        &mut self,
        ep_obj_id: u32,
        space_id: AddressSpaceId,
        out_buf: usize,
        out_cap: usize,
        handles_out: usize,
        handles_cap: usize,
    ) -> Option<Result<u64, SyscallError>> {
        let ep = self.endpoints.get_mut(ep_obj_id)?;
        let (call, reply_cap) = ep.dequeue_call()?;

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
        let staged = self.remove_handles_atomic(space_id, handles_ptr, handles_count)?;
        let ep = self
            .endpoints
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let (caller_id, caller_reply_buf) = ep.consume_reply(reply_cap_id)?;

        user_mem::write_user_bytes(caller_reply_buf, reply_msg.as_bytes())?;

        if staged.count > 0 {
            let caller_space = self
                .threads
                .get(caller_id.0)
                .ok_or(SyscallError::InvalidArgument)?
                .address_space()
                .ok_or(SyscallError::InvalidArgument)?;
            let mut staged = staged;
            let caller_ht = self
                .spaces
                .get_mut(caller_space.0)
                .ok_or(SyscallError::InvalidHandle)?
                .handles_mut();

            for i in 0..staged.count as usize {
                if let Some(h) = staged.handles[i].take() {
                    caller_ht.install(h)?;
                }
            }
        }

        crate::sched::wake(self, caller_id, 0);

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
        let ep = self
            .endpoints
            .get_mut(ep_handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        ep.bind_event(event_obj_id)?;

        if ep.has_pending_calls()
            && let Some(event) = self.events.get_mut(event_obj_id.0)
        {
            let woken = event.signal(Endpoint::ENDPOINT_READABLE_BIT);

            for info in woken.as_slice() {
                crate::sched::wake(self, info.thread_id, 0);
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

        self.threads.get_mut(idx).unwrap().id = ThreadId(idx);
        self.link_thread_to_space(idx, space_id);

        let core = self.scheduler.least_loaded_core();

        self.scheduler
            .enqueue(core, ThreadId(idx), Priority::Medium);

        let space = self
            .spaces
            .get_mut(space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Thread, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.unlink_thread_from_space(idx, space_id);
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

        self.threads.get_mut(idx).unwrap().id = ThreadId(idx);
        self.link_thread_to_space(idx, target_space);

        if handles_count > 0 {
            let mut cloned = [const { None }; config::MAX_IPC_HANDLES];
            {
                let caller_space = self
                    .spaces
                    .get(caller_space_id.0)
                    .ok_or(SyscallError::InvalidHandle)?;

                for (i, &hid) in handle_ids[..handles_count].iter().enumerate() {
                    cloned[i] = Some(caller_space.handles().lookup(HandleId(hid))?.clone());
                }
            }
            let target = self
                .spaces
                .get_mut(target_space.0)
                .ok_or(SyscallError::InvalidHandle)?;

            for (i, slot) in cloned[..handles_count].iter_mut().enumerate() {
                let h = slot.take().unwrap();

                if let Err(e) = target.handles_mut().allocate_at(i, h) {
                    for j in 0..i {
                        target.handles_mut().close(HandleId(j as u32)).ok();
                    }

                    self.unlink_thread_from_space(idx, target_space);
                    self.threads.dealloc(idx);

                    return Err(e);
                }
            }
        }

        let core = self.scheduler.least_loaded_core();

        self.scheduler
            .enqueue(core, ThreadId(idx), Priority::Medium);

        let space = self
            .spaces
            .get_mut(caller_space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;

        match space
            .handles_mut()
            .allocate(ObjectType::Thread, idx, Rights::ALL, generation)
        {
            Ok(hid) => Ok(hid.0 as u64),
            Err(e) => {
                self.unlink_thread_from_space(idx, target_space);
                self.threads.dealloc(idx);

                Err(e)
            }
        }
    }

    fn sys_thread_exit(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let code = args[0] as u32;

        crate::sched::exit_current(self, current, 0, code);

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

        // 1. Walk the thread list and kill all threads in the target space.
        let mut thread_cursor = self
            .spaces
            .get(target_id.0)
            .and_then(|s| s.thread_head());

        while let Some(tid) = thread_cursor {
            thread_cursor = self.threads.get(tid).and_then(|t| t.space_next());

            if let Some(t) = self.threads.get_mut(tid) {
                t.exit(0);
            }

            self.scheduler.remove(ThreadId(tid));
            self.threads.dealloc(tid);
        }

        // 2. Dealloc the space (drops handle table, mappings, VA allocator).
        // VMO refcount decrement and endpoint peer-closed signaling would go
        // here in a production kernel. For now, the space is simply dropped.
        self.spaces
            .dealloc(target_id.0)
            .ok_or(SyscallError::InvalidHandle)?;

        // 3. Close caller's handle.
        let caller_space = self
            .spaces
            .get_mut(caller_space_id.0)
            .ok_or(SyscallError::InvalidArgument)?;
        let _ = caller_space.handles_mut().close(handle_id);

        Ok(0)
    }

    // -- VMO cross-space + pager --

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
    use crate::types::Priority;

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

        k
    }

    fn call(k: &mut Kernel, num: u64, args: &[u64; 6]) -> (u64, u64) {
        k.dispatch(ThreadId(0), num, args)
    }

    #[test]
    fn unknown_syscall() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, 999, &[0; 6]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);
    }

    #[test]
    fn vmo_create_and_close() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.vmos.count(), 1);

        let (err, _) = call(&mut k, num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
    }

    #[test]
    fn vmo_create_zero_size() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);
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
    }

    #[test]
    fn event_clear() {
        let mut k = setup_kernel();
        let (err, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        call(&mut k, num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);
        call(&mut k, num::EVENT_CLEAR, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(k.events.get(0).unwrap().bits(), 0b10);
    }

    #[test]
    fn endpoint_create() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.endpoints.count(), 1);
    }

    #[test]
    fn space_create() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(k.spaces.count(), 2);
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
    }

    #[test]
    fn handle_info_returns_type_and_rights() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, info) = call(&mut k, num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let obj_type = (info >> 32) as u8;

        assert_eq!(obj_type, ObjectType::Event as u8);
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
    }

    #[test]
    fn vmo_snapshot_through_syscall() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, snap_hid) = call(&mut k, num::VMO_SNAPSHOT, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_ne!(hid, snap_hid);
        assert_eq!(k.vmos.count(), 2);
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
    }

    #[test]
    fn wrong_handle_type_rejected() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);
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
    }

    #[test]
    fn event_bind_irq_wrong_handle_type() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[vmo_hid, 64, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);
    }

    #[test]
    fn event_bind_irq_invalid_intid() {
        let mut k = setup_kernel();
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::EVENT_BIND_IRQ, &[event_hid, 10, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);
    }

    #[test]
    fn event_clear_non_irq_skips_scan() {
        let mut k = setup_kernel();
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        call(&mut k, num::EVENT_SIGNAL, &[event_hid, 0b11, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::EVENT_CLEAR, &[event_hid, 0b11, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.events.get(0).unwrap().bits(), 0);
    }

    // -- New syscall tests --

    #[test]
    fn event_wait_returns_immediately_if_bits_set() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);

        let (err, packed) = call(&mut k, num::EVENT_WAIT, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let fired = packed & 0xFFFF_FFFF;
        let which_hid = packed >> 32;

        assert_eq!(fired, 0b01);
        assert_eq!(which_hid, hid);
    }

    #[test]
    fn event_multi_wait_first_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid1, 0b1, 0, 0, 0, 0]);

        let (err, packed) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b1, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(packed >> 32, hid1);
        assert_eq!(packed & 0xFFFF_FFFF, 0b1);
    }

    #[test]
    fn event_multi_wait_second_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid2, 0b10, 0, 0, 0, 0]);

        let (err, packed) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b10, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(packed >> 32, hid2);
        assert_eq!(packed & 0xFFFF_FFFF, 0b10);
    }

    #[test]
    fn event_multi_wait_three_events_middle_fires() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (_, hid3) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid2, 0b100, 0, 0, 0, 0]);

        let (err, packed) = call(
            &mut k,
            num::EVENT_WAIT,
            &[hid1, 0b1, hid2, 0b100, hid3, 0b10],
        );

        assert_eq!(err, 0);
        assert_eq!(packed >> 32, hid2);
        assert_eq!(packed & 0xFFFF_FFFF, 0b100);
    }

    #[test]
    fn event_wait_zero_mask_skipped() {
        let mut k = setup_kernel();
        let (_, hid1) = call(&mut k, num::EVENT_CREATE, &[0; 6]);

        call(&mut k, num::EVENT_SIGNAL, &[hid1, 0b1, 0, 0, 0, 0]);

        let (err, packed) = call(&mut k, num::EVENT_WAIT, &[hid1, 0b1, 999, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(packed >> 32, hid1);
    }

    #[test]
    fn thread_create_and_inspect() {
        let mut k = setup_kernel();
        let (err, _tid_handle) = call(&mut k, num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(k.threads.count(), 2);
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
    }

    #[test]
    fn space_destroy_invalid_handle() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[999, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);
    }

    #[test]
    fn space_destroy_kills_threads() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        assert_eq!(k.spaces.count(), 2);

        let space_id = k
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
        assert!(k.threads.get(space_id).is_none() || k.threads.count() < initial_threads);
    }

    #[test]
    fn space_destroy_double_returns_error() {
        let mut k = setup_kernel();
        let (_, space_hid) = call(&mut k, num::SPACE_CREATE, &[0; 6]);

        call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(&mut k, num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);
    }

    #[test]
    fn system_info_page_size() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 16384);
    }

    #[test]
    fn system_info_msg_size() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 128);
    }

    #[test]
    fn system_info_num_cores() {
        let mut k = setup_kernel();
        let (err, val) = call(&mut k, num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 1);
    }

    #[test]
    fn vmo_set_pager() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(&mut k, num::ENDPOINT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::VMO_SET_PAGER, &[vmo_hid, ep_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);
    }

    #[test]
    fn thread_exit() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::THREAD_EXIT, &[42, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
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

        let (err, _) = k.dispatch(ThreadId(0), num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]);

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
    }
}
