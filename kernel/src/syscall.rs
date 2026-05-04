//! Syscall dispatch — maps syscall numbers to kernel object operations.
//!
//! The Kernel struct owns all object tables and the scheduler. Each syscall
//! handler extracts arguments, validates handles/rights, calls the kernel
//! object method, and returns (error_code, return_value).

use alloc::vec::Vec;

use crate::{
    address_space::AddressSpace,
    config,
    endpoint::{Endpoint, Message, PendingCall, ReplyCapId},
    event::Event,
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
    pub const IRQ_BIND: u64 = 28;
    pub const IRQ_ACK: u64 = 29;
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
    next_asid: u8,
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
            next_asid: 1,
        }
    }

    pub fn alloc_asid(&mut self) -> Result<u8, SyscallError> {
        if self.next_asid as usize >= config::MAX_ADDRESS_SPACES {
            return Err(SyscallError::OutOfMemory);
        }
        let asid = self.next_asid;
        self.next_asid += 1;
        Ok(asid)
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
            num::IRQ_BIND => self.sys_irq_bind(current, args),
            num::IRQ_ACK => self.sys_irq_ack(args),
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
        let idx = self.vmos.alloc(vmo).ok_or(SyscallError::OutOfMemory)?;
        self.vmos.get_mut(idx).unwrap().id = VmoId(idx);

        let generation = self.vmos.get(idx).unwrap().generation();
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
        let idx = self.vmos.alloc(snap).ok_or(SyscallError::OutOfMemory)?;
        self.vmos.get_mut(idx).unwrap().id = VmoId(idx);
        self.vmos
            .get(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        let generation = self.vmos.get(idx).unwrap().generation();
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
        let idx = self.endpoints.alloc(ep).ok_or(SyscallError::OutOfMemory)?;
        self.endpoints.get_mut(idx).unwrap().id = EndpointId(idx);

        let generation = self.endpoints.get(idx).unwrap().generation();
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
        let idx = self.events.alloc(event).ok_or(SyscallError::OutOfMemory)?;
        self.events.get_mut(idx).unwrap().id = EventId(idx);

        let generation = self.events.get(idx).unwrap().generation();
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

        self.events
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?
            .clear(bits);
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
        let idx = self.spaces.alloc(space).ok_or(SyscallError::OutOfMemory)?;
        self.spaces.get_mut(idx).unwrap().id = AddressSpaceId(idx);

        let generation = self.spaces.get(idx).unwrap().generation();
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
        let handle_id = HandleId(args[0] as u32);
        let mask = args[1];

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;
        if handle.object_type != ObjectType::Event {
            return Err(SyscallError::WrongHandleType);
        }
        if !handle.rights.contains(Rights::WAIT) {
            return Err(SyscallError::InsufficientRights);
        }

        let event = self
            .events
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Some(fired) = event.check(mask) {
            return Ok(fired);
        }

        event.add_waiter(current, mask)?;
        let obj_id = handle.object_id;
        crate::sched::block_current(self, current, 0);

        let event = self.events.get(obj_id).ok_or(SyscallError::InvalidHandle)?;
        Ok(event.check(mask).unwrap_or(0))
    }

    // -- IPC blocking --

    fn sys_call(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;
        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let priority = self
            .threads
            .get(current.0)
            .ok_or(SyscallError::InvalidArgument)?
            .effective_priority();

        let call = PendingCall {
            caller: current,
            priority,
            message: Message::empty(),
            handles: Vec::new(),
            badge: 0,
        };

        let ep = self
            .endpoints
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        let recv_waiters = ep.drain_recv_waiters();
        ep.enqueue_call(call)?;

        for waiter in recv_waiters.as_slice() {
            crate::sched::wake(self, *waiter, 0);
        }

        crate::sched::block_current(self, current, 0);
        Ok(0)
    }

    fn sys_recv(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;
        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let obj_id = handle.object_id;
        let ep = self
            .endpoints
            .get_mut(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Some((_call, reply_cap)) = ep.dequeue_call() {
            return Ok(reply_cap.0 as u64);
        }

        if ep.is_peer_closed() {
            return Err(SyscallError::PeerClosed);
        }

        ep.add_recv_waiter(current)?;
        crate::sched::block_current(self, current, 0);

        let ep = self
            .endpoints
            .get_mut(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Some((_call, reply_cap)) = ep.dequeue_call() {
            return Ok(reply_cap.0 as u64);
        }

        Err(SyscallError::PeerClosed)
    }

    fn sys_reply(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let handle_id = HandleId(args[0] as u32);
        let reply_cap_id = ReplyCapId(args[1] as u32);

        let space_id = self.thread_space_id(current)?;
        let handle = self.lookup_handle(space_id, handle_id)?;
        if handle.object_type != ObjectType::Endpoint {
            return Err(SyscallError::WrongHandleType);
        }

        let ep = self
            .endpoints
            .get_mut(handle.object_id)
            .ok_or(SyscallError::InvalidHandle)?;

        let caller_id = ep.consume_reply(reply_cap_id)?;
        crate::sched::wake(self, caller_id, 0);

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
        let idx = self
            .threads
            .alloc(thread)
            .ok_or(SyscallError::OutOfMemory)?;
        self.threads.get_mut(idx).unwrap().id = ThreadId(idx);

        let core = self.scheduler.least_loaded_core();
        self.scheduler
            .enqueue(core, ThreadId(idx), Priority::Medium);

        let generation = self.threads.get(idx).unwrap().generation();
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

        let caller_space_id = self.thread_space_id(current)?;
        let space_handle = self.lookup_handle(caller_space_id, space_handle_id)?;
        if space_handle.object_type != ObjectType::AddressSpace {
            return Err(SyscallError::WrongHandleType);
        }
        if !space_handle.rights.contains(Rights::SPAWN) {
            return Err(SyscallError::InsufficientRights);
        }

        let target_space = AddressSpaceId(space_handle.object_id);

        let thread = Thread::new(
            ThreadId(0),
            Some(target_space),
            Priority::Medium,
            entry,
            stack_top,
            arg,
        );
        let idx = self
            .threads
            .alloc(thread)
            .ok_or(SyscallError::OutOfMemory)?;
        self.threads.get_mut(idx).unwrap().id = ThreadId(idx);

        let core = self.scheduler.least_loaded_core();
        self.scheduler
            .enqueue(core, ThreadId(idx), Priority::Medium);

        let generation = self.threads.get(idx).unwrap().generation();
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

        self.spaces
            .dealloc(target_id.0)
            .ok_or(SyscallError::InvalidHandle)?;

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

    fn sys_irq_bind(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let intid = args[0] as u32;
        let handle_id = HandleId(args[1] as u32);
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

    fn sys_irq_ack(&mut self, args: &[u64; 6]) -> Result<u64, SyscallError> {
        let intid = args[0] as u32;
        self.irqs.ack(intid)?;
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
    fn irq_bind_and_ack() {
        let mut k = setup_kernel();
        let (err, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        assert_eq!(err, 0);

        let (err, _) = call(&mut k, num::IRQ_BIND, &[64, event_hid, 0b1010, 0, 0, 0]);
        assert_eq!(err, 0);

        let sig = k.irqs.handle_irq(64).unwrap();
        assert_eq!(sig.event_id, EventId(0));
        assert_eq!(sig.signal_bits, 0b1010);

        let (err, _) = call(&mut k, num::IRQ_ACK, &[64, 0, 0, 0, 0, 0]);
        assert_eq!(err, 0);
    }

    #[test]
    fn irq_bind_wrong_handle_type() {
        let mut k = setup_kernel();
        let (_, vmo_hid) = call(&mut k, num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(&mut k, num::IRQ_BIND, &[64, vmo_hid, 0b1, 0, 0, 0]);
        assert_eq!(err, SyscallError::WrongHandleType as u64);
    }

    #[test]
    fn irq_bind_invalid_intid() {
        let mut k = setup_kernel();
        let (_, event_hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(&mut k, num::IRQ_BIND, &[10, event_hid, 0b1, 0, 0, 0]);
        assert_eq!(err, SyscallError::InvalidArgument as u64);
    }

    #[test]
    fn irq_ack_without_pending() {
        let mut k = setup_kernel();
        let (err, _) = call(&mut k, num::IRQ_ACK, &[64, 0, 0, 0, 0, 0]);
        assert_eq!(err, SyscallError::NotFound as u64);
    }

    // -- New syscall tests --

    #[test]
    fn event_wait_returns_immediately_if_bits_set() {
        let mut k = setup_kernel();
        let (_, hid) = call(&mut k, num::EVENT_CREATE, &[0; 6]);
        call(&mut k, num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);
        let (err, fired) = call(&mut k, num::EVENT_WAIT, &[hid, 0b01, 0, 0, 0, 0]);
        assert_eq!(err, 0);
        assert_eq!(fired, 0b01);
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
}
