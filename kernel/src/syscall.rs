//! Syscall dispatch — maps syscall numbers to kernel object operations.
//!
//! Free functions that access global kernel state through `frame::state`.
//! Each syscall handler acquires only the per-object locks it needs from
//! the global `ConcurrentTable` instances — no global kernel lock.

use crate::{
    address_space::AddressSpace,
    config,
    endpoint::{Endpoint, PendingCall, ReplyCapId},
    event::Event,
    frame::{state, user_mem},
    handle::Handle,
    thread::Thread,
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

// ── Resource monitoring ─────────────────────────────────────

#[cfg(any(test, fuzzing, debug_assertions))]
pub fn resource_pressure() -> [(&'static str, usize, usize); 5] {
    [
        ("vmos", state::vmos().count(), config::MAX_VMOS),
        ("events", state::events().count(), config::MAX_EVENTS),
        (
            "endpoints",
            state::endpoints().count(),
            config::MAX_ENDPOINTS,
        ),
        ("threads", state::threads().count(), config::MAX_THREADS),
        (
            "spaces",
            state::spaces().count(),
            config::MAX_ADDRESS_SPACES,
        ),
    ]
}

// ── Thread register initialization ──────────────────────────

#[cfg(any(target_os = "none", test))]
fn init_thread_registers(thread: &mut Thread, entry: usize, stack_top: usize, arg: usize) {
    let rs = thread.init_register_state();

    rs.pc = entry as u64;
    rs.sp = stack_top as u64;
    rs.gprs[0] = arg as u64;
    // SPSR_EL1 = 0: EL0t, all interrupts unmasked, AArch64
    rs.pstate = 0;

    #[cfg(target_os = "none")]
    {
        rs.gprs[30] = crate::frame::arch::context::new_thread_trampoline() as u64;
    }
}

#[cfg(not(any(target_os = "none", test)))]
fn init_thread_registers(_thread: &mut Thread, _entry: usize, _sp: usize, _arg: usize) {}

// ── Kernel stack management ─────────────────────────────────

#[cfg(target_os = "none")]
fn free_kernel_stack(thread_idx: u32) {
    let base = state::threads()
        .read(thread_idx)
        .unwrap()
        .kernel_stack_base();

    if base != 0 {
        let base_pa = crate::frame::arch::platform::virt_to_phys(base);

        for i in 0..config::KERNEL_STACK_PAGES {
            crate::frame::arch::page_alloc::release(crate::frame::arch::page_alloc::PhysAddr(
                base_pa + i * config::PAGE_SIZE,
            ));
        }
    }
}

// ── Thread ↔ space linked list ──────────────────────────────

fn link_thread_to_space(thread_idx: u32, space_id: AddressSpaceId) {
    let old_head = state::spaces()
        .read(space_id.0)
        .and_then(|s| s.thread_head());

    if let Some(mut t) = state::threads().write(thread_idx) {
        t.set_space_next(old_head);
        t.set_space_prev(None);
    }

    if let Some(old) = old_head
        && let Some(mut t) = state::threads().write(old)
    {
        t.set_space_prev(Some(thread_idx));
    }

    if let Some(mut s) = state::spaces().write(space_id.0) {
        s.set_thread_head(Some(thread_idx));
    }
}

fn unlink_thread_from_space(thread_idx: u32, space_id: AddressSpaceId) {
    let (prev, next) = state::threads()
        .read(thread_idx)
        .map_or((None, None), |t| (t.space_prev(), t.space_next()));

    if let Some(p) = prev {
        if let Some(mut t) = state::threads().write(p) {
            t.set_space_next(next);
        }
    } else if let Some(mut s) = state::spaces().write(space_id.0) {
        s.set_thread_head(next);
    }

    if let Some(n) = next
        && let Some(mut t) = state::threads().write(n)
    {
        t.set_space_prev(prev);
    }

    if let Some(mut t) = state::threads().write(thread_idx) {
        t.set_space_next(None);
        t.set_space_prev(None);
    }
}

// ── Handle lookup ───────────────────────────────────────────

#[inline]
pub fn thread_space_id(thread: ThreadId) -> Result<AddressSpaceId, SyscallError> {
    state::threads()
        .read(thread.0)
        .ok_or(SyscallError::InvalidArgument)?
        .address_space()
        .ok_or(SyscallError::InvalidArgument)
}

#[inline]
fn lookup_handle(space_id: AddressSpaceId, handle_id: HandleId) -> Result<Handle, SyscallError> {
    let handle = state::spaces()
        .read(space_id.0)
        .ok_or(SyscallError::InvalidHandle)?
        .handles()
        .lookup(handle_id)?
        .clone();

    let current_gen = match handle.object_type {
        ObjectType::Vmo => state::vmos().generation(handle.object_id),
        ObjectType::Endpoint => state::endpoints().generation(handle.object_id),
        ObjectType::Event => state::events().generation(handle.object_id),
        ObjectType::Thread => state::threads().generation(handle.object_id),
        ObjectType::AddressSpace => state::spaces().generation(handle.object_id),
    };

    if handle.generation != current_gen {
        return Err(SyscallError::GenerationMismatch);
    }

    Ok(handle)
}

#[inline]
fn lookup_endpoint_id(space_id: AddressSpaceId, handle_id: HandleId) -> Result<u32, SyscallError> {
    let (obj_type, obj_id, handle_gen) = {
        let space = state::spaces()
            .read(space_id.0)
            .ok_or(SyscallError::InvalidHandle)?;
        let handle = space.handles().lookup(handle_id)?;

        (handle.object_type, handle.object_id, handle.generation)
    };

    if obj_type != ObjectType::Endpoint {
        return Err(SyscallError::WrongHandleType);
    }

    if handle_gen != state::endpoints().generation(obj_id) {
        return Err(SyscallError::GenerationMismatch);
    }

    Ok(obj_id)
}

// ── Handle transfer helpers ─────────────────────────────────

fn remove_handles_atomic(
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

    let mut space = state::spaces()
        .write(space_id.0)
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

fn reinstall_handles(space_id: AddressSpaceId, mut staged: StagedHandles) {
    if let Some(mut space) = state::spaces().write(space_id.0) {
        let ht = space.handles_mut();

        for i in 0..staged.count as usize {
            if let Some(h) = staged.handles[i].take() {
                let _ = ht.install(h);
            }
        }
    }
}

fn install_handles(
    space_id: AddressSpaceId,
    staged: &mut StagedHandles,
    out_ptr: usize,
    out_cap: usize,
) -> Result<usize, SyscallError> {
    let count = staged.count as usize;

    if count > out_cap {
        return Err(SyscallError::BufferFull);
    }

    let mut space = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidHandle)?;
    let ht = space.handles_mut();
    let mut new_ids = [0u32; config::MAX_IPC_HANDLES];

    for (slot, out_id) in staged.handles[..count].iter_mut().zip(new_ids.iter_mut()) {
        let h = slot.take().unwrap();

        *out_id = ht.install(h)?.0;
    }

    drop(space);

    user_mem::write_user_u32s(out_ptr, &new_ids[..count])?;

    Ok(count)
}

// ── Reference counting ──────────────────────────────────────

fn add_object_ref(object_type: ObjectType, object_id: u32) {
    match object_type {
        ObjectType::Vmo => {
            if let Some(vmo) = state::vmos().read(object_id) {
                vmo.add_ref();
            }
        }
        ObjectType::Endpoint => {
            if let Some(ep) = state::endpoints().read(object_id) {
                ep.add_ref();
            }
        }
        ObjectType::Event => {
            if let Some(evt) = state::events().read(object_id) {
                evt.add_ref();
            }
        }
        ObjectType::Thread | ObjectType::AddressSpace => {}
    }
}

fn release_object_ref(object_type: ObjectType, object_id: u32, core_id: usize) {
    match object_type {
        ObjectType::Vmo => {
            let should_destroy = state::vmos()
                .read(object_id)
                .is_some_and(|vmo| vmo.release_ref());

            if should_destroy {
                let has_mappings = state::vmos()
                    .read(object_id)
                    .is_some_and(|vmo| vmo.mapping_count() > 0);

                if has_mappings {
                    let vmo_id = VmoId(object_id);

                    state::spaces().for_each_mut(|_, space| {
                        space.remove_mappings_for_vmo(vmo_id);
                    });
                }

                state::vmos().dealloc_shared(object_id);
            }
        }
        ObjectType::Endpoint => {
            let should_destroy = state::endpoints()
                .read(object_id)
                .is_some_and(|ep| ep.release_ref());

            if should_destroy {
                close_endpoint_peer(object_id, core_id);

                let bound_event = state::endpoints()
                    .read(object_id)
                    .and_then(|ep| ep.bound_event());

                if let Some(evt_id) = bound_event
                    && let Some(mut evt) = state::events().write(evt_id.0)
                {
                    evt.unbind_endpoint();
                }

                state::endpoints().dealloc_shared(object_id);
            }
        }
        ObjectType::Event => {
            let should_destroy = state::events()
                .read(object_id)
                .is_some_and(|evt| evt.release_ref());

            if should_destroy {
                destroy_event(object_id);
            }
        }
        ObjectType::Thread | ObjectType::AddressSpace => {}
    }
}

fn close_endpoint_peer(ep_id: u32, core_id: usize) {
    let mut close_result = {
        let Some(mut ep) = state::endpoints().write(ep_id) else {
            return;
        };

        match ep.close_peer() {
            Some(cr) => cr,
            None => return,
        }
    };

    for canceled in close_result.canceled_callers_mut() {
        if let Some(caller) = canceled.take() {
            if caller.handle_count > 0 {
                let caller_space = state::threads()
                    .read(caller.thread_id.0)
                    .and_then(|t| t.address_space());

                if let Some(sid) = caller_space {
                    reinstall_handles(
                        sid,
                        StagedHandles {
                            handles: caller.handles,
                            count: caller.handle_count,
                        },
                    );
                }
            }

            if let Some(mut t) = state::threads().write(caller.thread_id.0) {
                t.set_wakeup_error(SyscallError::PeerClosed);

                #[cfg(any(target_os = "none", test))]
                if let Some(rs) = t.register_state_mut() {
                    rs.gprs[0] = SyscallError::PeerClosed as u64;
                }
            }

            crate::sched::wake(caller.thread_id, core_id);
        }
    }

    for &tid in close_result.reply_callers() {
        if let Some(mut t) = state::threads().write(tid.0) {
            t.set_wakeup_error(SyscallError::PeerClosed);

            #[cfg(any(target_os = "none", test))]
            if let Some(rs) = t.register_state_mut() {
                rs.gprs[0] = SyscallError::PeerClosed as u64;
            }
        }

        crate::sched::wake(tid, core_id);
    }

    for &tid in close_result.recv_waiters() {
        if let Some(mut t) = state::threads().write(tid.0) {
            t.set_wakeup_error(SyscallError::PeerClosed);
        }

        crate::sched::wake(tid, core_id);
    }
}

fn destroy_event(event_id: u32) {
    let bound_ep = state::events()
        .read(event_id)
        .and_then(|evt| evt.bound_endpoint());

    if let Some(ep_id) = bound_ep
        && let Some(mut ep) = state::endpoints().write(ep_id.0)
    {
        ep.unbind_event();
    }

    if state::irqs().lock().has_bindings() {
        for intid in 0..config::MAX_IRQS {
            let is_bound = state::irqs()
                .lock()
                .binding_at(intid)
                .is_some_and(|b| b.event_id.0 == event_id);

            if is_bound {
                let _ = state::irqs().lock().unbind(intid as u32);
            }
        }
    }

    state::events().dealloc_shared(event_id);
}

fn check_clear_readable(ep: &Endpoint) -> Option<(EventId, u64)> {
    if ep.has_pending_calls() {
        return None;
    }

    ep.bound_event()
        .map(|eid| (eid, Endpoint::ENDPOINT_READABLE_BIT))
}

// ── Main dispatch ───────────────────────────────────────────

#[inline(never)]
pub fn dispatch(
    current: ThreadId,
    core_id: usize,
    syscall_num: u64,
    args: &[u64; 6],
) -> (u64, u64) {
    let result = match syscall_num {
        num::VMO_CREATE => sys_vmo_create(current, core_id, args),
        num::VMO_MAP => sys_vmo_map(current, core_id, args),
        num::VMO_MAP_INTO => sys_vmo_map_into(current, core_id, args),
        num::VMO_UNMAP => sys_vmo_unmap(current, core_id, args),
        num::VMO_SNAPSHOT => sys_vmo_snapshot(current, core_id, args),
        num::VMO_SEAL => sys_vmo_seal(current, core_id, args),
        num::VMO_RESIZE => sys_vmo_resize(current, core_id, args),
        num::VMO_SET_PAGER => sys_vmo_set_pager(current, core_id, args),
        num::ENDPOINT_CREATE => sys_endpoint_create(current, core_id, args),
        num::CALL => sys_call(current, core_id, args),
        num::RECV => sys_recv(current, core_id, args),
        num::REPLY => sys_reply(current, core_id, args),
        num::EVENT_CREATE => sys_event_create(current, core_id, args),
        num::EVENT_SIGNAL => sys_event_signal(current, core_id, args),
        num::EVENT_WAIT => sys_event_wait(current, core_id, args),
        num::EVENT_CLEAR => sys_event_clear(current, core_id, args),
        num::THREAD_CREATE => sys_thread_create(current, core_id, args),
        num::THREAD_CREATE_IN => sys_thread_create_in(current, core_id, args),
        num::THREAD_EXIT => sys_thread_exit(current, core_id, args),
        num::THREAD_SET_PRIORITY => sys_thread_set_priority(current, core_id, args),
        num::THREAD_SET_AFFINITY => sys_thread_set_affinity(current, core_id, args),
        num::SPACE_CREATE => sys_space_create(current, core_id, args),
        num::SPACE_DESTROY => sys_space_destroy(current, core_id, args),
        num::HANDLE_DUP => sys_handle_dup(current, core_id, args),
        num::HANDLE_CLOSE => sys_handle_close(current, core_id, args),
        num::HANDLE_INFO => sys_handle_info(current, core_id, args),
        num::CLOCK_READ => sys_clock_read(current, core_id, args),
        num::SYSTEM_INFO => sys_system_info(current, core_id, args),
        num::EVENT_BIND_IRQ => sys_event_bind_irq(current, core_id, args),
        num::ENDPOINT_BIND_EVENT => sys_endpoint_bind_event(current, core_id, args),
        _ => Err(SyscallError::InvalidArgument),
    };

    let outcome = match result {
        Ok(value) => (0, value),
        Err(e) => (e as u64, 0),
    };

    #[cfg(all(debug_assertions, target_os = "none"))]
    {
        let violations = crate::invariants::verify();

        if !violations.is_empty() {
            crate::println!("INVARIANT VIOLATION after syscall {syscall_num}:");

            for v in &violations {
                crate::println!("  {v}");
            }

            panic!("kernel invariant violated");
        }

        for &(name, count, max) in &resource_pressure() {
            if count * 4 > max * 3 {
                crate::println!(
                    "RESOURCE PRESSURE: {name} at {count}/{max} ({}%)",
                    count * 100 / max
                );
            }
        }
    }

    outcome
}

// ── VMO syscalls ────────────────────────────────────────────

#[inline(never)]
fn sys_vmo_create(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let size = args[0] as usize;
    let flags = args[1] as u32;

    if size == 0 || size > config::MAX_PHYS_MEM {
        return Err(SyscallError::InvalidArgument);
    }

    let known_flags = VmoFlags::HINT_CONTIGUOUS.0;

    if flags & !known_flags != 0 {
        return Err(SyscallError::InvalidArgument);
    }

    let space_id = thread_space_id(current)?;
    let vmo = Vmo::new(VmoId(0), size, VmoFlags(flags));
    let (idx, generation) = state::vmos()
        .alloc_shared(vmo)
        .ok_or(SyscallError::OutOfMemory)?;

    state::vmos().write(idx).unwrap().id = VmoId(idx);

    let hid = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Vmo, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => Ok(hid.0 as u64),
        Err(e) => {
            state::vmos().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_vmo_map(current: ThreadId, _core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let addr_hint = args[1] as usize;
    let perms = Rights(args[2] as u32);
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::MAP) {
        return Err(SyscallError::InsufficientRights);
    }
    if perms.contains(Rights::WRITE) && !handle.rights.contains(Rights::WRITE) {
        return Err(SyscallError::InsufficientRights);
    }

    let vmo_id = handle.object_id;
    let vmo_size = state::vmos()
        .read(vmo_id)
        .ok_or(SyscallError::InvalidHandle)?
        .size();
    let va = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .map_vmo(VmoId(vmo_id), vmo_size, perms, addr_hint)?;

    state::vmos()
        .write(vmo_id)
        .ok_or(SyscallError::InvalidHandle)?
        .inc_mapping_count();

    Ok(va as u64)
}

#[inline(never)]
fn sys_vmo_unmap(current: ThreadId, _core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let addr = args[0] as usize;
    let space_id = thread_space_id(current)?;
    let record = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .unmap(addr)?;

    if let Some(mut vmo) = state::vmos().write(record.vmo_id.0) {
        vmo.dec_mapping_count();
    }

    Ok(0)
}

#[inline(never)]
fn sys_vmo_snapshot(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }

    let snap = state::vmos()
        .read(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .snapshot(VmoId(0));
    let (idx, generation) = state::vmos()
        .alloc_shared(snap)
        .ok_or(SyscallError::OutOfMemory)?;

    state::vmos().write(idx).unwrap().id = VmoId(idx);

    let hid = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Vmo, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => Ok(hid.0 as u64),
        Err(e) => {
            state::vmos().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_vmo_seal(current: ThreadId, _core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::WRITE) {
        return Err(SyscallError::InsufficientRights);
    }

    state::vmos()
        .write(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .seal()?;

    Ok(0)
}

#[inline(never)]
fn sys_vmo_resize(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let new_size = args[1] as usize;

    if new_size > config::MAX_PHYS_MEM {
        return Err(SyscallError::InvalidArgument);
    }

    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::WRITE) {
        return Err(SyscallError::InsufficientRights);
    }

    let vmo_id = handle.object_id;
    let aligned_new = new_size.next_multiple_of(config::PAGE_SIZE);

    for i in 0..config::MAX_ADDRESS_SPACES as u32 {
        if let Some(space) = state::spaces().read(i) {
            for m in space.mappings() {
                if m.vmo_id.0 == vmo_id && m.size > aligned_new {
                    return Err(SyscallError::InvalidArgument);
                }
            }
        }
    }

    state::vmos()
        .write(vmo_id)
        .ok_or(SyscallError::InvalidHandle)?
        .resize(new_size, |_pa| {
            #[cfg(target_os = "none")]
            crate::frame::arch::page_alloc::release(crate::frame::arch::page_alloc::PhysAddr(_pa));
        })?;

    Ok(0)
}

#[inline(never)]
fn sys_vmo_map_into(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let vmo_handle_id = HandleId(args[0] as u32);
    let space_handle_id = HandleId(args[1] as u32);
    let addr_hint = args[2] as usize;
    let perms = Rights(args[3] as u32);
    let space_id = thread_space_id(current)?;
    let vmo_handle = lookup_handle(space_id, vmo_handle_id)?;

    if vmo_handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }
    if !vmo_handle.rights.contains(Rights::MAP) {
        return Err(SyscallError::InsufficientRights);
    }

    let space_handle = lookup_handle(space_id, space_handle_id)?;

    if space_handle.object_type != ObjectType::AddressSpace {
        return Err(SyscallError::WrongHandleType);
    }

    let vmo_id = vmo_handle.object_id;
    let vmo_size = state::vmos()
        .read(vmo_id)
        .ok_or(SyscallError::InvalidHandle)?
        .size();
    let va = state::spaces()
        .write(space_handle.object_id)
        .ok_or(SyscallError::InvalidArgument)?
        .map_vmo(VmoId(vmo_id), vmo_size, perms, addr_hint)?;

    state::vmos()
        .write(vmo_id)
        .ok_or(SyscallError::InvalidHandle)?
        .inc_mapping_count();

    Ok(va as u64)
}

#[inline(never)]
fn sys_vmo_set_pager(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let vmo_handle_id = HandleId(args[0] as u32);
    let ep_handle_id = HandleId(args[1] as u32);
    let space_id = thread_space_id(current)?;
    let vmo_handle = lookup_handle(space_id, vmo_handle_id)?;

    if vmo_handle.object_type != ObjectType::Vmo {
        return Err(SyscallError::WrongHandleType);
    }
    if !vmo_handle.rights.contains(Rights::WRITE) {
        return Err(SyscallError::InsufficientRights);
    }

    let ep_handle = lookup_handle(space_id, ep_handle_id)?;

    if ep_handle.object_type != ObjectType::Endpoint {
        return Err(SyscallError::WrongHandleType);
    }

    state::vmos()
        .write(vmo_handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .set_pager(EndpointId(ep_handle.object_id))?;

    Ok(0)
}

// ── Endpoint syscalls ───────────────────────────────────────

#[inline(never)]
fn sys_endpoint_create(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let _ = args;
    let space_id = thread_space_id(current)?;
    let ep = Endpoint::new(EndpointId(0));
    let (idx, generation) = state::endpoints()
        .alloc_shared(ep)
        .ok_or(SyscallError::OutOfMemory)?;

    state::endpoints().write(idx).unwrap().id = EndpointId(idx);

    let hid = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Endpoint, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => Ok(hid.0 as u64),
        Err(e) => {
            state::endpoints().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_endpoint_bind_event(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let ep_handle_id = HandleId(args[0] as u32);
    let event_handle_id = HandleId(args[1] as u32);
    let space_id = thread_space_id(current)?;
    let ep_handle = lookup_handle(space_id, ep_handle_id)?;

    if ep_handle.object_type != ObjectType::Endpoint {
        return Err(SyscallError::WrongHandleType);
    }
    if !ep_handle.rights.contains(Rights::WRITE) {
        return Err(SyscallError::InsufficientRights);
    }

    let event_handle = lookup_handle(space_id, event_handle_id)?;

    if event_handle.object_type != ObjectType::Event {
        return Err(SyscallError::WrongHandleType);
    }
    if !event_handle.rights.contains(Rights::SIGNAL) {
        return Err(SyscallError::InsufficientRights);
    }

    let event_obj_id = EventId(event_handle.object_id);
    let ep_obj_id = ep_handle.object_id;

    state::endpoints()
        .write(ep_obj_id)
        .ok_or(SyscallError::InvalidHandle)?
        .bind_event(event_obj_id)?;

    {
        let mut event = state::events()
            .write(event_obj_id.0)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Err(e) = event.bind_endpoint(EndpointId(ep_obj_id)) {
            drop(event);

            if let Some(mut ep) = state::endpoints().write(ep_obj_id) {
                ep.unbind_event();
            }

            return Err(e);
        }
    }

    let has_pending = state::endpoints()
        .read(ep_obj_id)
        .is_some_and(|ep| ep.has_pending_calls());

    if has_pending && let Some(mut event) = state::events().write(event_obj_id.0) {
        let woken = event.signal(Endpoint::ENDPOINT_READABLE_BIT);

        for info in woken.as_slice() {
            crate::sched::wake(info.thread_id, core_id);
        }
    }

    Ok(0)
}

// ── Event syscalls ──────────────────────────────────────────

#[inline(never)]
fn sys_event_create(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let _ = args;
    let space_id = thread_space_id(current)?;
    let event = Event::new(EventId(0));
    let (idx, generation) = state::events()
        .alloc_shared(event)
        .ok_or(SyscallError::OutOfMemory)?;

    state::events().write(idx).unwrap().id = EventId(idx);

    let hid = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Event, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => Ok(hid.0 as u64),
        Err(e) => {
            state::events().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_event_signal(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let bits = args[1];
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Event {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::SIGNAL) {
        return Err(SyscallError::InsufficientRights);
    }

    let woken = state::events()
        .write(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .signal(bits);

    for info in woken.as_slice() {
        crate::sched::wake(info.thread_id, core_id);
    }

    Ok(0)
}

#[inline(never)]
fn sys_event_clear(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let bits = args[1];
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Event {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::SIGNAL) {
        return Err(SyscallError::InsufficientRights);
    }

    state::events()
        .write(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .clear(bits);

    let (intids, count) = state::irqs()
        .lock()
        .intids_for_event_bits(EventId(handle.object_id), bits);

    for &intid in &intids[..count] {
        if state::irqs().lock().ack(intid).is_ok() {
            #[cfg(target_os = "none")]
            crate::frame::arch::gic::unmask_spi(intid);
        }
    }

    Ok(0)
}

#[inline(never)]
fn sys_event_bind_irq(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let intid = args[1] as u32;
    let signal_bits = args[2];
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Event {
        return Err(SyscallError::WrongHandleType);
    }
    if !handle.rights.contains(Rights::SIGNAL) {
        return Err(SyscallError::InsufficientRights);
    }

    let event_id = EventId(handle.object_id);

    state::irqs().lock().bind(intid, event_id, signal_bits)?;

    Ok(0)
}

// ── Event blocking ──────────────────────────────────────────

#[inline(never)]
fn sys_event_wait(current: ThreadId, core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let space_id = thread_space_id(current)?;

    if args[0] as u32 > config::MAX_HANDLES as u32 {
        return event_wait_buffer(current, core_id, space_id, args);
    }

    event_wait_register(current, core_id, space_id, args)
}

fn event_wait_register(
    current: ThreadId,
    core_id: usize,
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

        let handle = lookup_handle(space_id, HandleId(hid_raw))?;

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

    event_wait_common(current, core_id, &wait_items[..count])
}

fn event_wait_buffer(
    current: ThreadId,
    core_id: usize,
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

        let handle = lookup_handle(space_id, HandleId(hid_raw))?;

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

    event_wait_common(current, core_id, &wait_items[..valid])
}

fn event_wait_common(
    current: ThreadId,
    core_id: usize,
    wait_items: &[(u32, u32, u64)],
) -> Result<u64, SyscallError> {
    for &(hid, obj_id, mask) in wait_items {
        let event = state::events()
            .write(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if event.check(mask).is_some() {
            return Ok(hid as u64);
        }
    }

    let mut obj_ids = [0u32; config::MAX_MULTI_WAIT];
    let use_count = wait_items.len().min(config::MAX_MULTI_WAIT);

    for (i, &(_, obj_id, mask)) in wait_items[..use_count].iter().enumerate() {
        let mut event = state::events()
            .write(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if let Err(e) = event.add_waiter(current, mask) {
            drop(event);

            for &prev_id in &obj_ids[..i] {
                if let Some(mut prev_event) = state::events().write(prev_id) {
                    prev_event.remove_waiter(current);
                }
            }

            return Err(e);
        }

        obj_ids[i] = obj_id;
    }

    state::threads()
        .write(current.0)
        .ok_or(SyscallError::InvalidArgument)?
        .set_wait_events(&obj_ids[..use_count]);

    crate::sched::block_current(current, core_id);

    let (wait_evts, wait_n) = state::threads()
        .write(current.0)
        .ok_or(SyscallError::InvalidArgument)?
        .take_wait_events();

    for i in 0..wait_n as usize {
        let obj_id = wait_evts[i];
        let Some(&(hid, _, mask)) = wait_items.iter().find(|&&(_, oid, _)| oid == obj_id) else {
            continue;
        };
        let Some(event) = state::events().write(obj_id) else {
            continue;
        };

        if event.check(mask).is_some() {
            drop(event);

            for &evt_id in &wait_evts[..wait_n as usize] {
                if evt_id != obj_id
                    && let Some(mut e) = state::events().write(evt_id)
                {
                    e.remove_waiter(current);
                }
            }

            return Ok(hid as u64);
        }
    }

    for &evt_id in &wait_evts[..wait_n as usize] {
        if let Some(mut e) = state::events().write(evt_id) {
            e.remove_waiter(current);
        }
    }

    Ok(0)
}

// ── IPC blocking ────────────────────────────────────────────

#[inline(never)]
fn sys_call(current: ThreadId, core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let msg_ptr = args[1] as usize;
    let msg_len = args[2] as usize;
    let handles_ptr = args[3] as usize;
    let handles_count = args[4] as usize;

    if handles_count > config::MAX_IPC_HANDLES {
        return Err(SyscallError::InvalidArgument);
    }

    let space_id = thread_space_id(current)?;
    let ep_obj_id = lookup_endpoint_id(space_id, handle_id)?;

    {
        let ep = state::endpoints()
            .read(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if ep.is_peer_closed() {
            return Err(SyscallError::PeerClosed);
        }
        if ep.is_full() {
            return Err(SyscallError::BufferFull);
        }
    }

    let message = user_mem::read_user_message(msg_ptr, msg_len)?;
    let staged = remove_handles_atomic(space_id, handles_ptr, handles_count)?;
    // ── Fast path: direct transfer when a server is waiting ──
    let server_tid = state::endpoints()
        .write(ep_obj_id)
        .ok_or(SyscallError::InvalidHandle)?
        .pop_recv_waiter();

    if let Some(server_tid) = server_tid {
        let recv_state = state::threads()
            .write(server_tid.0)
            .and_then(|mut t| t.take_recv_state());

        if recv_state.is_none()
            && let Some(mut ep) = state::endpoints().write(ep_obj_id)
        {
            let _ = ep.add_recv_waiter(server_tid);
        }

        if let Some(rs) = recv_state {
            let msg_bytes = message.as_bytes();

            if msg_bytes.len() <= rs.out_cap {
                let _ = user_mem::write_user_bytes(rs.out_buf, msg_bytes);
            }

            let msg_len_val = msg_bytes.len() as u64;
            let h_count = if staged.count > 0 {
                let mut staged = staged;

                install_handles(rs.space_id, &mut staged, rs.handles_out, rs.handles_cap)
                    .unwrap_or(0) as u64
            } else {
                0
            };
            let reply_cap = {
                let mut ep = state::endpoints()
                    .write(ep_obj_id)
                    .ok_or(SyscallError::InvalidHandle)?;
                let reply_cap = ep.allocate_reply_cap(current, msg_ptr);

                ep.set_active_server(Some(server_tid));

                reply_cap
            };

            if let Some(cap_id) = reply_cap {
                if rs.reply_cap_out != 0 {
                    let _ = user_mem::write_user_u64(rs.reply_cap_out, cap_id.0);
                }

                let packed = (h_count << 16) | msg_len_val;

                if let Some(mut server) = state::threads().write(server_tid.0) {
                    server.set_wakeup_value(packed);
                }
            }

            let caller_pri = state::threads()
                .read(current.0)
                .map_or(Priority::Idle, |t| t.effective_priority());

            if let Some(mut server) = state::threads().write(server_tid.0) {
                server.boost_priority(caller_pri);
            }

            crate::sched::wake(server_tid, core_id);
            crate::sched::block_current(current, core_id);

            if let Some(err) = state::threads()
                .write(current.0)
                .and_then(|mut t| t.take_wakeup_error())
            {
                return Err(err);
            }

            return Ok(0);
        }
    }

    // ── Slow path: enqueue into priority send queue ──────────
    let priority = state::threads()
        .read(current.0)
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
    let (signal_info, active_server, recv_waiters) = {
        let mut ep = state::endpoints()
            .write(ep_obj_id)
            .ok_or(SyscallError::InvalidHandle)?;
        let signal_info = ep
            .enqueue_call(call)
            .expect("enqueue_call failed after pre-check passed");
        let active_server = ep.active_server();
        let recv_waiters = ep.drain_recv_waiters();

        (signal_info, active_server, recv_waiters)
    };

    if let Some(server_tid) = active_server {
        let caller_pri = state::threads()
            .read(current.0)
            .map_or(Priority::Idle, |t| t.effective_priority());

        if let Some(mut server) = state::threads().write(server_tid.0) {
            server.boost_priority(caller_pri);
        }
    }

    if let Some((event_id, bits)) = signal_info
        && let Some(mut event) = state::events().write(event_id.0)
    {
        let woken = event.signal(bits);

        for info in woken.as_slice() {
            crate::sched::wake(info.thread_id, core_id);
        }
    }

    for waiter in recv_waiters.as_slice() {
        crate::sched::wake(*waiter, core_id);
    }

    crate::sched::block_current(current, core_id);

    if let Some(err) = state::threads()
        .write(current.0)
        .and_then(|mut t| t.take_wakeup_error())
    {
        return Err(err);
    }

    Ok(0)
}

#[inline(never)]
fn sys_recv(current: ThreadId, core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let out_buf = args[1] as usize;
    let out_cap = args[2] as usize;
    let handles_out = args[3] as usize;
    let handles_cap = args[4] as usize;
    let reply_cap_out = args[5] as usize;
    let space_id = thread_space_id(current)?;
    let obj_id = lookup_endpoint_id(space_id, handle_id)?;

    if let Some(result) = try_dequeue_and_deliver(
        obj_id,
        current,
        space_id,
        out_buf,
        out_cap,
        handles_out,
        handles_cap,
        reply_cap_out,
    ) {
        return result;
    }

    if let Some(val) = state::threads()
        .write(current.0)
        .and_then(|mut t| t.take_wakeup_value())
    {
        return Ok(val);
    }

    {
        let ep = state::endpoints()
            .read(obj_id)
            .ok_or(SyscallError::InvalidHandle)?;

        if ep.is_peer_closed() {
            return Err(SyscallError::PeerClosed);
        }
    }

    state::endpoints()
        .write(obj_id)
        .ok_or(SyscallError::InvalidHandle)?
        .add_recv_waiter(current)?;

    if let Some(mut t) = state::threads().write(current.0) {
        t.set_recv_state(crate::thread::RecvState {
            endpoint_id: obj_id,
            space_id,
            out_buf,
            out_cap,
            handles_out,
            handles_cap,
            reply_cap_out,
        });
    }

    crate::sched::block_current(current, core_id);

    if let Some(mut t) = state::threads().write(current.0) {
        t.take_recv_state();
    }

    if let Some(err) = state::threads()
        .write(current.0)
        .and_then(|mut t| t.take_wakeup_error())
    {
        return Err(err);
    }

    if let Some(val) = state::threads()
        .write(current.0)
        .and_then(|mut t| t.take_wakeup_value())
    {
        return Ok(val);
    }

    if let Some(result) = try_dequeue_and_deliver(
        obj_id,
        current,
        space_id,
        out_buf,
        out_cap,
        handles_out,
        handles_cap,
        reply_cap_out,
    ) {
        return result;
    }

    if state::endpoints()
        .read(obj_id)
        .is_some_and(|ep| !ep.is_peer_closed())
    {
        return Err(SyscallError::TimedOut);
    }

    Err(SyscallError::PeerClosed)
}

#[allow(clippy::too_many_arguments)]
fn try_dequeue_and_deliver(
    ep_obj_id: u32,
    server: ThreadId,
    space_id: AddressSpaceId,
    out_buf: usize,
    out_cap: usize,
    handles_out: usize,
    handles_cap: usize,
    reply_cap_out: usize,
) -> Option<Result<u64, SyscallError>> {
    let (call, reply_cap, clear_info) = {
        let mut ep = state::endpoints().write(ep_obj_id)?;
        let (call, reply_cap) = ep.dequeue_call()?;

        ep.set_active_server(Some(server));

        let clear_info = check_clear_readable(&ep);

        (call, reply_cap, clear_info)
    };

    if let Some((eid, bits)) = clear_info
        && let Some(mut e) = state::events().write(eid.0)
    {
        e.clear(bits);
    }

    Some(recv_deliver(
        space_id,
        call,
        reply_cap,
        out_buf,
        out_cap,
        handles_out,
        handles_cap,
        reply_cap_out,
    ))
}

#[allow(clippy::too_many_arguments)]
fn recv_deliver(
    space_id: AddressSpaceId,
    mut call: PendingCall,
    reply_cap: ReplyCapId,
    out_buf: usize,
    out_cap: usize,
    handles_out: usize,
    handles_cap: usize,
    reply_cap_out: usize,
) -> Result<u64, SyscallError> {
    let msg_bytes = call.message.as_bytes();

    if msg_bytes.len() > out_cap {
        return Err(SyscallError::BufferFull);
    }

    user_mem::write_user_bytes(out_buf, msg_bytes)?;

    if reply_cap_out != 0 {
        user_mem::write_user_u64(reply_cap_out, reply_cap.0)?;
    }

    let msg_len = msg_bytes.len() as u64;
    let mut staged = StagedHandles {
        handles: core::mem::replace(&mut call.handles, [const { None }; config::MAX_IPC_HANDLES]),
        count: call.handle_count,
    };
    let h_count = if staged.count > 0 {
        install_handles(space_id, &mut staged, handles_out, handles_cap)? as u64
    } else {
        0
    };

    Ok((call.badge as u64) << 32 | (h_count << 16) | msg_len)
}

#[inline(never)]
fn sys_reply(current: ThreadId, core_id: usize, args: &[u64; 6]) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let reply_cap_id = ReplyCapId(args[1]);
    let msg_ptr = args[2] as usize;
    let msg_len = args[3] as usize;
    let handles_ptr = args[4] as usize;
    let handles_count = args[5] as usize;

    if handles_count > config::MAX_IPC_HANDLES {
        return Err(SyscallError::InvalidArgument);
    }

    let space_id = thread_space_id(current)?;
    let ep_obj_id = lookup_endpoint_id(space_id, handle_id)?;
    let reply_msg = user_mem::read_user_message(msg_ptr, msg_len)?;
    let (caller_id, caller_reply_buf) = state::endpoints()
        .write(ep_obj_id)
        .ok_or(SyscallError::InvalidHandle)?
        .consume_reply(reply_cap_id)?;
    let next_highest = state::endpoints()
        .read(ep_obj_id)
        .and_then(|ep| ep.highest_caller_priority());

    if let Some(pri) = next_highest {
        if let Some(mut server) = state::threads().write(current.0) {
            server.boost_priority(pri);
        }
    } else if let Some(mut server) = state::threads().write(current.0) {
        server.release_boost();
    }

    let caller_state = state::threads()
        .read(caller_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .state();

    if caller_state != crate::thread::ThreadRunState::Blocked {
        return Err(SyscallError::InvalidArgument);
    }

    if handles_count > 0 {
        let caller_space_id = state::threads()
            .read(caller_id.0)
            .ok_or(SyscallError::InvalidArgument)?
            .address_space()
            .ok_or(SyscallError::InvalidArgument)?;
        let free_slots = state::spaces()
            .read(caller_space_id.0)
            .ok_or(SyscallError::InvalidHandle)?
            .handles()
            .free_slot_count();

        if free_slots < handles_count {
            return Err(SyscallError::BufferFull);
        }
    }

    let staged = remove_handles_atomic(space_id, handles_ptr, handles_count)?;

    if let Err(e) = user_mem::write_user_bytes(caller_reply_buf, reply_msg.as_bytes()) {
        reinstall_handles(space_id, staged);

        return Err(e);
    }

    if staged.count > 0 {
        let caller_space_id = state::threads()
            .read(caller_id.0)
            .ok_or(SyscallError::InvalidArgument)?
            .address_space()
            .ok_or(SyscallError::InvalidArgument)?;
        let mut staged = staged;
        let mut caller_space = state::spaces()
            .write(caller_space_id.0)
            .ok_or(SyscallError::InvalidHandle)?;
        let caller_ht = caller_space.handles_mut();

        for i in 0..staged.count as usize {
            if let Some(h) = staged.handles[i].take() {
                let result = caller_ht.install(h);

                debug_assert!(result.is_ok(), "handle install failed despite pre-check");
            }
        }
    }

    crate::sched::wake(caller_id, core_id);

    Ok(reply_msg.len() as u64)
}

// ── Space syscalls ──────────────────────────────────────────

#[inline(never)]
fn sys_space_create(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let _ = args;
    let caller_space_id = thread_space_id(current)?;
    let asid = state::alloc_asid()?;
    let space = AddressSpace::new(AddressSpaceId(0), asid, 0);
    let Some((idx, generation)) = state::spaces().alloc_shared(space) else {
        state::free_asid(asid);
        return Err(SyscallError::OutOfMemory);
    };

    state::spaces().write(idx).unwrap().id = AddressSpaceId(idx);
    #[cfg(target_os = "none")]
    state::spaces()
        .write(idx)
        .unwrap()
        .set_aslr_seed(crate::frame::arch::entropy::random_u64());

    let hid = state::spaces()
        .write(caller_space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::AddressSpace, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => Ok(hid.0 as u64),
        Err(e) => {
            let asid = state::spaces().read(idx).map_or(0, |s| s.asid());

            state::spaces().dealloc_shared(idx);
            state::free_asid(asid);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_space_destroy(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let caller_space_id = thread_space_id(current)?;
    let handle = lookup_handle(caller_space_id, handle_id)?;

    if handle.object_type != ObjectType::AddressSpace {
        return Err(SyscallError::WrongHandleType);
    }

    let target_id = AddressSpaceId(handle.object_id);

    if target_id == caller_space_id {
        return Err(SyscallError::InvalidArgument);
    }

    if state::spaces().read(target_id.0).is_none() {
        return Err(SyscallError::InvalidHandle);
    }

    // 1. Kill all threads in the target space.
    let mut thread_cursor = state::spaces()
        .read(target_id.0)
        .and_then(|s| s.thread_head());

    while let Some(tid) = thread_cursor {
        thread_cursor = state::threads().read(tid).and_then(|t| t.space_next());

        let (wait_evts, wait_n) = state::threads()
            .write(tid)
            .map_or(([0; config::MAX_MULTI_WAIT], 0), |mut t| {
                t.take_wait_events()
            });

        for &evt_id in &wait_evts[..wait_n as usize] {
            if let Some(mut e) = state::events().write(evt_id) {
                e.remove_waiter(ThreadId(tid));
            }
        }

        state::endpoints().for_each_mut(|_, ep| {
            ep.remove_recv_waiter(ThreadId(tid));
        });

        #[cfg(target_os = "none")]
        free_kernel_stack(tid);

        if let Some(mut t) = state::threads().write(tid)
            && t.state() != crate::thread::ThreadRunState::Exited
        {
            t.exit(0);
            state::dec_alive_threads();
        }

        state::schedulers().remove(ThreadId(tid));
    }

    // 2. Walk the handle table and clean up referenced objects.
    let mut handle_buf = [(ObjectType::Vmo, 0u32); config::MAX_HANDLES];
    let mut handle_count = 0;

    if let Some(space) = state::spaces().read(target_id.0) {
        for (_, h) in space.handles().iter_handles() {
            if handle_count < config::MAX_HANDLES {
                handle_buf[handle_count] = (h.object_type, h.object_id);
                handle_count += 1;
            }
        }
    }

    for &(obj_type, obj_id) in &handle_buf[..handle_count] {
        if obj_type == ObjectType::Endpoint {
            let is_open = state::endpoints()
                .read(obj_id)
                .is_some_and(|ep| !ep.is_peer_closed());

            if is_open {
                close_endpoint_peer(obj_id, core_id);
            }
        }

        release_object_ref(obj_type, obj_id, core_id);
    }

    // 3. Free page table and ASID.
    #[cfg(target_os = "none")]
    if let Some(space) = state::spaces().read(target_id.0) {
        let asid = space.asid();
        let root = space.page_table_root();

        drop(space);

        if root != 0 {
            crate::frame::arch::page_table::destroy_page_table(
                crate::frame::arch::page_alloc::PhysAddr(root),
                crate::frame::arch::page_table::Asid(asid),
            );
        }
    }

    #[cfg(all(not(target_os = "none"), test))]
    if let Some(space) = state::spaces().read(target_id.0) {
        let asid = space.asid();

        drop(space);

        if asid != 0 {
            crate::frame::arch::page_table::free_asid(crate::frame::arch::page_table::Asid(asid));
        }
    }

    // 4. Dealloc the space.
    if !state::spaces().dealloc_shared(target_id.0) {
        return Err(SyscallError::InvalidHandle);
    }

    // 5. Close caller's handle to the destroyed space.
    if let Some(mut caller) = state::spaces().write(caller_space_id.0) {
        let _ = caller.handles_mut().close(handle_id);
    }

    Ok(0)
}

// ── Handle syscalls ─────────────────────────────────────────

#[inline(never)]
fn sys_handle_dup(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let new_rights = Rights(args[1] as u32);
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if !handle.rights.contains(Rights::DUP) {
        return Err(SyscallError::InsufficientRights);
    }

    let obj_type = handle.object_type;
    let obj_id = handle.object_id;
    let new_id = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .duplicate(handle_id, new_rights)?;

    add_object_ref(obj_type, obj_id);

    Ok(new_id.0 as u64)
}

#[inline(never)]
fn sys_handle_close(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let space_id = thread_space_id(current)?;
    let handle = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .close(handle_id)?;

    release_object_ref(handle.object_type, handle.object_id, core_id);

    Ok(0)
}

#[inline(never)]
fn sys_handle_info(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    Ok(((handle.object_type as u64) << 32) | (handle.rights.0 as u64))
}

// ── Miscellaneous syscalls ──────────────────────────────────

#[inline(never)]
fn sys_clock_read(
    _current: ThreadId,
    _core_id: usize,
    _args: &[u64; 6],
) -> Result<u64, SyscallError> {
    #[cfg(any(target_os = "none", test))]
    {
        const FREQ: u64 = crate::frame::arch::timer::TIMER_FREQ_HZ;

        let ticks = crate::frame::arch::timer::now();
        let secs = ticks / FREQ;
        let remainder = ticks % FREQ;

        Ok(secs * 1_000_000_000 + remainder * 1_000_000_000 / FREQ)
    }

    #[cfg(not(any(target_os = "none", test)))]
    Ok(0)
}

#[inline(never)]
fn sys_system_info(
    _current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let what = args[0];

    match what {
        0 => Ok(crate::config::PAGE_SIZE as u64),
        1 => Ok(crate::endpoint::MSG_SIZE as u64),
        2 => Ok(state::schedulers().num_cores() as u64),
        _ => Err(SyscallError::InvalidArgument),
    }
}

// ── Thread lifecycle ────────────────────────────────────────

#[inline(never)]
fn sys_thread_create(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let entry = args[0] as usize;
    let stack_top = args[1] as usize;
    let arg = args[2] as usize;
    let space_id = thread_space_id(current)?;
    let thread = Thread::new(
        ThreadId(0),
        Some(space_id),
        Priority::Medium,
        entry,
        stack_top,
        arg,
    );
    let (idx, generation) = state::threads()
        .alloc_shared(thread)
        .ok_or(SyscallError::OutOfMemory)?;

    {
        let mut t = state::threads().write(idx).unwrap();

        t.id = ThreadId(idx);

        init_thread_registers(&mut t, entry, stack_top, arg);
    }

    #[cfg(target_os = "none")]
    {
        let ks = crate::frame::arch::context::alloc_kernel_stack();

        if let Some((base, top)) = ks {
            let mut t = state::threads().write(idx).unwrap();

            t.set_kernel_stack(base, top);
            t.init_register_state().kernel_sp = top as u64;
        } else {
            state::threads().dealloc_shared(idx);

            return Err(SyscallError::OutOfMemory);
        }
    }

    link_thread_to_space(idx, space_id);

    let hid = state::spaces()
        .write(space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Thread, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => {
            state::schedulers()
                .core(core_id)
                .lock()
                .enqueue(ThreadId(idx), Priority::Medium);
            state::inc_alive_threads();

            Ok(hid.0 as u64)
        }
        Err(e) => {
            unlink_thread_from_space(idx, space_id);
            #[cfg(target_os = "none")]
            free_kernel_stack(idx);

            state::threads().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_thread_create_in(
    current: ThreadId,
    _core_id: usize,
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

    let caller_space_id = thread_space_id(current)?;
    let space_handle = lookup_handle(caller_space_id, space_handle_id)?;

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
    let (idx, generation) = state::threads()
        .alloc_shared(thread)
        .ok_or(SyscallError::OutOfMemory)?;

    {
        let mut t = state::threads().write(idx).unwrap();

        t.id = ThreadId(idx);

        init_thread_registers(&mut t, entry, stack_top, arg);
    }

    #[cfg(target_os = "none")]
    {
        let ks = crate::frame::arch::context::alloc_kernel_stack();

        if let Some((base, top)) = ks {
            let mut t = state::threads().write(idx).unwrap();

            t.set_kernel_stack(base, top);
            t.init_register_state().kernel_sp = top as u64;
        } else {
            state::threads().dealloc_shared(idx);

            return Err(SyscallError::OutOfMemory);
        }
    }

    link_thread_to_space(idx, target_space);

    if handles_count > 0 {
        let mut cloned = [const { None }; config::MAX_IPC_HANDLES];
        let clone_result: Result<(), SyscallError> = (|| {
            let caller_space = state::spaces()
                .read(caller_space_id.0)
                .ok_or(SyscallError::InvalidHandle)?;

            for (i, &hid) in handle_ids[..handles_count].iter().enumerate() {
                cloned[i] = Some(caller_space.handles().lookup(HandleId(hid))?.clone());
            }

            Ok(())
        })();

        if let Err(e) = clone_result {
            unlink_thread_from_space(idx, target_space);
            #[cfg(target_os = "none")]
            free_kernel_stack(idx);

            state::threads().dealloc_shared(idx);

            return Err(e);
        }

        let mut installed_refs = [(ObjectType::Vmo, 0u32); config::MAX_IPC_HANDLES];
        let mut installed_count = 0;
        let install_result: Result<(), SyscallError> = (|| {
            let mut target = state::spaces()
                .write(target_space.0)
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
                release_object_ref(obj_type, obj_id, _core_id);
            }

            unlink_thread_from_space(idx, target_space);
            #[cfg(target_os = "none")]
            free_kernel_stack(idx);

            state::threads().dealloc_shared(idx);

            return Err(e);
        }

        for &(obj_type, obj_id) in &installed_refs[..installed_count] {
            add_object_ref(obj_type, obj_id);
        }
    }

    let hid = state::spaces()
        .write(caller_space_id.0)
        .ok_or(SyscallError::InvalidArgument)?
        .handles_mut()
        .allocate(ObjectType::Thread, idx, Rights::ALL, generation);

    match hid {
        Ok(hid) => {
            let core = state::schedulers().least_loaded_core();

            state::schedulers()
                .core(core)
                .lock()
                .enqueue(ThreadId(idx), Priority::Medium);
            state::inc_alive_threads();

            Ok(hid.0 as u64)
        }
        Err(e) => {
            unlink_thread_from_space(idx, target_space);

            #[cfg(target_os = "none")]
            free_kernel_stack(idx);

            state::threads().dealloc_shared(idx);

            Err(e)
        }
    }
}

#[inline(never)]
fn sys_thread_exit(
    current: ThreadId,
    core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let code = args[0] as u32;

    crate::sched::exit_current(current, core_id, code);

    #[allow(unused_variables)]
    let remaining = state::dec_alive_threads();

    #[cfg(target_os = "none")]
    if remaining == 0 {
        #[cfg(feature = "bench-smp")]
        {
            print_smp_bench_results(args);

            crate::frame::arch::psci::system_off();
        }

        #[cfg(feature = "bench-el0")]
        {
            print_el0_bench_results(args);

            crate::frame::arch::psci::system_off();
        }

        #[cfg(feature = "integration-tests")]
        {
            crate::println!("INTEGRATION TEST: EXIT {code}");
            crate::frame::arch::psci::system_off();
        }

        #[cfg(not(any(
            feature = "integration-tests",
            feature = "bench-el0",
            feature = "bench-smp"
        )))]
        loop {
            crate::frame::arch::halt();
        }
    }

    Ok(0)
}

#[cfg(feature = "bench-el0")]
fn print_el0_bench_results(args: &[u64; 6]) {
    let batch_n = args[5] as usize;

    if batch_n == 0 {
        crate::println!("EL0 BENCH: no results (batch_n=0)");

        return;
    }

    crate::println!("--- EL0 cycle estimates ({}x, 24MHz->4.5GHz) ---", batch_n);

    let benches: [(&str, u64, u64); 4] = [
        ("svc null (EL0 fast path)", args[1], 50),
        ("clock_read (full syscall)", args[2], 55),
        ("handle_info (full syscall)", args[3], 55),
        ("event_signal (full syscall)", args[4], 65),
    ];

    for (name, total_ticks, theoretical) in &benches {
        let cycles_x10 = total_ticks * 1875 / batch_n as u64;
        let ratio_x10 = cycles_x10.checked_div(*theoretical).unwrap_or(0);

        crate::println!(
            "  {:36} {:>5}.{} cyc  (floor ~{:>3})  {}.{}x",
            name,
            cycles_x10 / 10,
            cycles_x10 % 10,
            theoretical,
            ratio_x10 / 10,
            ratio_x10 % 10,
        );
    }
}

#[cfg(feature = "bench-smp")]
fn print_smp_bench_results(args: &[u64; 6]) {
    let batch_n = (args[5] & 0xFFFF) as usize;
    let workers = ((args[5] >> 16) & 0xFFFF) as usize;

    if batch_n == 0 {
        crate::println!("SMP BENCH: no results (batch_n=0)");

        return;
    }

    crate::println!(
        "--- SMP benchmarks ({}x, {} cores, 24MHz->4.5GHz) ---",
        batch_n,
        workers,
    );

    let ipc_ticks = args[1];
    let churn_1 = args[2];
    let churn_n = args[3];
    let wake_ticks = args[4];

    if ipc_ticks > 0 {
        let cyc = ipc_ticks * 1875 / batch_n as u64;

        crate::println!(
            "  {:36} {:>5}.{} cyc/rtt",
            "IPC null round-trip (2-core)",
            cyc / 10,
            cyc % 10,
        );
    }

    if churn_1 > 0 {
        let cyc1 = churn_1 * 1875 / batch_n as u64;

        crate::println!(
            "  {:36} {:>5}.{} cyc/iter",
            "object churn (1-core)",
            cyc1 / 10,
            cyc1 % 10,
        );

        if churn_n > 0 && workers > 0 {
            let cyc_n = churn_n * 1875 / batch_n as u64;
            let scaling_x10 = if churn_n > 0 {
                churn_1 * workers as u64 * 10 / churn_n
            } else {
                0
            };

            crate::println!(
                "  {:36} {:>5}.{} cyc/iter  scaling {}.{}x / {}",
                "object churn (multi-core wall)",
                cyc_n / 10,
                cyc_n % 10,
                scaling_x10 / 10,
                scaling_x10 % 10,
                workers,
            );
        }
    }

    if wake_ticks > 0 {
        let cyc = wake_ticks * 1875 / batch_n as u64;
        let one_way = cyc / 2;

        crate::println!(
            "  {:36} {:>5}.{} cyc/rtt  (~{}.{} one-way)",
            "cross-core wake (event ping-pong)",
            cyc / 10,
            cyc % 10,
            one_way / 10,
            one_way % 10,
        );
    }

    crate::println!("--- end SMP benchmarks ---");
}

#[inline(never)]
fn sys_thread_set_priority(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let priority_val = args[1] as u8;
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

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

    state::threads()
        .write(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .set_priority(priority);

    Ok(0)
}

#[inline(never)]
fn sys_thread_set_affinity(
    current: ThreadId,
    _core_id: usize,
    args: &[u64; 6],
) -> Result<u64, SyscallError> {
    let handle_id = HandleId(args[0] as u32);
    let hint_val = args[1] as u8;
    let space_id = thread_space_id(current)?;
    let handle = lookup_handle(space_id, handle_id)?;

    if handle.object_type != ObjectType::Thread {
        return Err(SyscallError::WrongHandleType);
    }

    let hint = match hint_val {
        0 => crate::types::TopologyHint::Any,
        1 => crate::types::TopologyHint::Performance,
        2 => crate::types::TopologyHint::Efficiency,
        _ => return Err(SyscallError::InvalidArgument),
    };

    state::threads()
        .write(handle.object_id)
        .ok_or(SyscallError::InvalidHandle)?
        .set_affinity(hint);

    Ok(0)
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{thread::ThreadRunState, types::Priority};

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
            .set_state(ThreadRunState::Running);
        state::inc_alive_threads();
        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(0)));
    }

    fn call(num: u64, args: &[u64; 6]) -> (u64, u64) {
        dispatch(ThreadId(0), 0, num, args)
    }

    fn call_as(tid: ThreadId, num: u64, args: &[u64; 6]) -> (u64, u64) {
        dispatch(tid, 0, num, args)
    }

    fn assert_ok(result: (u64, u64)) -> u64 {
        assert_eq!(result.0, 0, "expected success, got error {}", result.0);

        result.1
    }

    #[allow(dead_code)]
    fn assert_err(result: (u64, u64), expected: SyscallError) {
        assert_eq!(
            result.0, expected as u64,
            "expected {:?} ({}), got {}",
            expected, expected as u64, result.0
        );
    }

    fn inv() {
        crate::invariants::assert_valid();
    }

    fn create_vmo() -> u64 {
        assert_ok(call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]))
    }

    fn create_event() -> u64 {
        assert_ok(call(num::EVENT_CREATE, &[0; 6]))
    }

    fn create_endpoint() -> u64 {
        assert_ok(call(num::ENDPOINT_CREATE, &[0; 6]))
    }

    fn create_thread() -> u64 {
        assert_ok(call(num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]))
    }

    fn create_space() -> u64 {
        assert_ok(call(num::SPACE_CREATE, &[0; 6]))
    }

    fn dup_with_rights(hid: u64, rights: u32) -> u64 {
        assert_ok(call(num::HANDLE_DUP, &[hid, rights as u64, 0, 0, 0, 0]))
    }

    fn create_stale_vmo_handle() -> u64 {
        let hid = create_vmo();
        let obj_id = state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        state::vmos().dealloc_shared(obj_id);

        let new_vmo = Vmo::new(VmoId(0), 8192, VmoFlags::NONE);

        state::vmos().alloc_shared(new_vmo).unwrap();

        hid
    }

    fn create_stale_event_handle() -> u64 {
        let hid = create_event();
        let obj_id = state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        state::events().dealloc_shared(obj_id);

        let new_event = Event::new(EventId(0));

        state::events().alloc_shared(new_event).unwrap();

        hid
    }

    fn create_stale_endpoint_handle() -> u64 {
        let hid = create_endpoint();
        let obj_id = state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        state::endpoints().dealloc_shared(obj_id);

        let new_ep = Endpoint::new(EndpointId(0));

        state::endpoints().alloc_shared(new_ep).unwrap();

        hid
    }

    fn create_stale_thread_handle() -> u64 {
        let hid = create_thread();
        let obj_id = state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        state::threads()
            .write(obj_id)
            .unwrap()
            .set_state(ThreadRunState::Exited);
        state::threads().dealloc_shared(obj_id);

        let new_thread = Thread::new(ThreadId(0), Some(AddressSpaceId(0)), Priority::Low, 0, 0, 0);

        state::threads().alloc_shared(new_thread).unwrap();

        hid
    }

    fn create_stale_space_handle() -> u64 {
        let hid = create_space();
        let obj_id = state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id;

        state::spaces().dealloc_shared(obj_id);

        let new_space = AddressSpace::new(AddressSpaceId(0), 99, 0);

        state::spaces().alloc_shared(new_space).unwrap();

        hid
    }

    fn do_call(ep_hid: u64, msg: &[u8], reply_buf: &mut [u8; 128]) {
        reply_buf[..msg.len()].copy_from_slice(msg);

        let (err, _) = call(
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

    fn do_recv(ep_hid: u64, out_buf: &mut [u8; 128]) -> (usize, u64) {
        let mut reply_cap: u64 = 0;
        let (err, packed) = call(
            num::RECV,
            &[
                ep_hid,
                out_buf.as_mut_ptr() as u64,
                128,
                0,
                0,
                &raw mut reply_cap as u64,
            ],
        );

        assert_eq!(err, 0, "RECV failed");

        let msg_len = (packed & 0xFFFF) as usize;

        (msg_len, reply_cap)
    }

    fn do_reply(ep_hid: u64, reply_cap: u64, msg: &[u8]) {
        let (err, _) = call(
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

    fn resume_caller() {
        let next = state::schedulers().core(0).lock().pick_next();

        if let Some(tid) = next {
            assert_eq!(tid, ThreadId(0));

            state::threads()
                .write(0)
                .unwrap()
                .set_state(ThreadRunState::Running);
            state::schedulers().core(0).lock().set_current(Some(tid));
        }
    }

    // ── Basic tests ─────────────────────────────────────────

    #[test]
    fn unknown_syscall() {
        setup();

        let (err, _) = call(999, &[0; 6]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        inv();
    }

    #[test]
    fn vmo_create_and_close() {
        setup();

        let (err, hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::vmos().count(), 1);

        let (err, _) = call(num::HANDLE_CLOSE, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    #[test]
    fn vmo_create_zero_size() {
        setup();

        let (err, _) = call(num::VMO_CREATE, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);
        inv();
    }

    #[test]
    fn event_create() {
        setup();

        let (err, hid) = call(num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(state::events().count(), 1);

        let (err, _) = call(num::EVENT_SIGNAL, &[hid, 0b101, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::events().read(0).unwrap().bits(), 0b101);

        inv();
    }

    #[test]
    fn event_clear() {
        setup();

        let (err, hid) = call(num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        call(num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);
        call(num::EVENT_CLEAR, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(state::events().read(0).unwrap().bits(), 0b10);

        inv();
    }

    #[test]
    fn endpoint_create() {
        setup();

        let (err, _) = call(num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(state::endpoints().count(), 1);

        inv();
    }

    #[test]
    fn space_create() {
        setup();

        let (err, _) = call(num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(state::spaces().count(), 2);

        inv();
    }

    #[test]
    fn handle_dup_with_reduced_rights() {
        setup();

        let (_, hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let read_only = Rights::READ.0 as u64;
        let (err, dup_hid) = call(num::HANDLE_DUP, &[hid, read_only, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_ne!(hid, dup_hid);

        let (_, info) = call(num::HANDLE_INFO, &[dup_hid, 0, 0, 0, 0, 0]);
        let rights = (info & 0xFFFF_FFFF) as u32;

        assert_eq!(rights, Rights::READ.0);

        inv();
    }

    #[test]
    fn handle_info_returns_type_and_rights() {
        setup();

        let (_, hid) = call(num::EVENT_CREATE, &[0; 6]);
        let (err, info) = call(num::HANDLE_INFO, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let obj_type = (info >> 32) as u8;

        assert_eq!(obj_type, ObjectType::Event as u8);

        inv();
    }

    #[test]
    fn vmo_seal_through_syscall() {
        setup();

        let (_, hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, _) = call(num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(state::vmos().read(0).unwrap().is_sealed());

        let (err, _) = call(num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::AlreadySealed as u64);

        inv();
    }

    #[test]
    fn vmo_snapshot_through_syscall() {
        setup();

        let (_, hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (err, snap_hid) = call(num::VMO_SNAPSHOT, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_ne!(hid, snap_hid);
        assert_eq!(state::vmos().count(), 2);

        inv();
    }

    #[test]
    fn vmo_map_and_unmap() {
        setup();

        let (_, hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let perms = (Rights::READ.0 | Rights::MAP.0) as u64;
        let (err, va) = call(num::VMO_MAP, &[hid, 0, perms, 0, 0, 0]);

        assert_eq!(err, 0);
        assert!(va > 0);

        let (err, _) = call(num::VMO_UNMAP, &[va, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    #[test]
    fn wrong_handle_type_rejected() {
        setup();

        let (_, hid) = call(num::EVENT_CREATE, &[0; 6]);
        let (err, _) = call(num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        inv();
    }

    #[test]
    fn event_bind_irq_and_clear_acks() {
        setup();

        let (err, event_hid) = call(num::EVENT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        let (err, _) = call(num::EVENT_BIND_IRQ, &[event_hid, 64, 0b1010, 0, 0, 0]);

        assert_eq!(err, 0);

        let sig = state::irqs().lock().handle_irq(64).unwrap();

        assert_eq!(sig.event_id, EventId(0));
        assert_eq!(sig.signal_bits, 0b1010);

        call(num::EVENT_SIGNAL, &[event_hid, 0b1010, 0, 0, 0, 0]);

        let (err, _) = call(num::EVENT_CLEAR, &[event_hid, 0b1010, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::events().read(0).unwrap().bits(), 0);

        inv();
    }

    #[test]
    fn event_bind_irq_wrong_handle_type() {
        setup();

        let vmo_hid = create_vmo();
        let (err, _) = call(num::EVENT_BIND_IRQ, &[vmo_hid, 64, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::WrongHandleType as u64);

        inv();
    }

    #[test]
    fn event_bind_irq_invalid_intid() {
        setup();

        let event_hid = create_event();
        let (err, _) = call(num::EVENT_BIND_IRQ, &[event_hid, 9999, 0b1, 0, 0, 0]);

        assert_eq!(err, SyscallError::InvalidArgument as u64);

        inv();
    }

    #[test]
    fn event_clear_non_irq_skips_scan() {
        setup();

        let event_hid = create_event();

        call(num::EVENT_SIGNAL, &[event_hid, 0b11, 0, 0, 0, 0]);

        let (err, _) = call(num::EVENT_CLEAR, &[event_hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::events().read(0).unwrap().bits(), 0b10);

        inv();
    }

    #[test]
    fn event_wait_returns_immediately_if_bits_set() {
        setup();

        let (_, hid) = call(num::EVENT_CREATE, &[0; 6]);

        call(num::EVENT_SIGNAL, &[hid, 0b11, 0, 0, 0, 0]);

        let (err, value) = call(num::EVENT_WAIT, &[hid, 0b01, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid);

        inv();
    }

    #[test]
    fn event_wait_with_upper_32_bits() {
        setup();

        let (_, hid) = call(num::EVENT_CREATE, &[0; 6]);
        let upper_bit: u64 = 1 << 48;

        call(num::EVENT_SIGNAL, &[hid, upper_bit, 0, 0, 0, 0]);

        let (err, value) = call(num::EVENT_WAIT, &[hid, upper_bit, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid);

        inv();
    }

    #[test]
    fn event_multi_wait_first_fires() {
        setup();

        let (_, hid1) = call(num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(num::EVENT_CREATE, &[0; 6]);

        call(num::EVENT_SIGNAL, &[hid1, 0b1, 0, 0, 0, 0]);

        let (err, value) = call(num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b1, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid1);

        inv();
    }

    #[test]
    fn event_multi_wait_second_fires() {
        setup();

        let (_, hid1) = call(num::EVENT_CREATE, &[0; 6]);
        let (_, hid2) = call(num::EVENT_CREATE, &[0; 6]);

        call(num::EVENT_SIGNAL, &[hid2, 0b10, 0, 0, 0, 0]);

        let (err, value) = call(num::EVENT_WAIT, &[hid1, 0b1, hid2, 0b10, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(value, hid2);

        inv();
    }

    #[test]
    fn thread_create_and_inspect() {
        setup();

        let (err, _tid_handle) = call(num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::threads().count(), 2);

        inv();
    }

    #[test]
    fn thread_set_priority() {
        setup();

        let (_, tid_handle) = call(num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);
        let (err, _) = call(num::THREAD_SET_PRIORITY, &[tid_handle, 3, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    #[test]
    fn thread_set_affinity() {
        setup();

        let (_, tid_handle) = call(num::THREAD_CREATE, &[0x1000, 0x2000, 0, 0, 0, 0]);
        let (err, _) = call(num::THREAD_SET_AFFINITY, &[tid_handle, 1, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    #[test]
    fn space_destroy() {
        setup();

        let (err, space_hid) = call(num::SPACE_CREATE, &[0; 6]);

        assert_eq!(err, 0);
        assert_eq!(state::spaces().count(), 2);

        let (err, _) = call(num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::spaces().count(), 1);

        inv();
    }

    #[test]
    fn space_destroy_invalid_handle() {
        setup();

        let (err, _) = call(num::SPACE_DESTROY, &[999, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);

        inv();
    }

    #[test]
    fn space_destroy_kills_threads() {
        setup();

        let (_, space_hid) = call(num::SPACE_CREATE, &[0; 6]);

        assert_eq!(state::spaces().count(), 2);

        let (_, _tid_hid) = call(num::THREAD_CREATE_IN, &[space_hid, 0x1000, 0x2000, 0, 0, 0]);
        let initial_threads = state::threads().count();
        let (err, _) = call(num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(state::spaces().count(), 1);

        let created_tid = initial_threads as u32 - 1;

        assert_eq!(
            state::threads().read(created_tid).unwrap().state(),
            crate::thread::ThreadRunState::Exited
        );

        inv();
    }

    #[test]
    fn space_destroy_double_returns_error() {
        setup();

        let (_, space_hid) = call(num::SPACE_CREATE, &[0; 6]);

        call(num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        let (err, _) = call(num::SPACE_DESTROY, &[space_hid, 0, 0, 0, 0, 0]);

        assert_ne!(err, 0);
        inv();
    }

    #[test]
    fn system_info_page_size() {
        setup();

        let (err, val) = call(num::SYSTEM_INFO, &[0, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 16384);

        inv();
    }

    #[test]
    fn system_info_msg_size() {
        setup();

        let (err, val) = call(num::SYSTEM_INFO, &[1, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 128);

        inv();
    }

    #[test]
    fn system_info_num_cores() {
        setup();

        let (err, val) = call(num::SYSTEM_INFO, &[2, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(val, 1);

        inv();
    }

    #[test]
    fn vmo_set_pager() {
        setup();

        let (_, vmo_hid) = call(num::VMO_CREATE, &[4096, 0, 0, 0, 0, 0]);
        let (_, ep_hid) = call(num::ENDPOINT_CREATE, &[0; 6]);
        let (err, _) = call(num::VMO_SET_PAGER, &[vmo_hid, ep_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    #[test]
    fn thread_exit() {
        setup();

        let (err, _) = call(num::THREAD_EXIT, &[42, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        inv();
    }

    // ── IPC tests ───────────────────────────────────────────

    fn setup_ipc() -> u64 {
        setup();

        let (err, ep_hid) = call(num::ENDPOINT_CREATE, &[0; 6]);

        assert_eq!(err, 0);

        ep_hid
    }

    #[test]
    fn ipc_send_recv_message_bytes() {
        let ep_hid = setup_ipc();
        let mut call_buf = [0u8; 128];
        let request = b"hello server";

        call_buf[..request.len()].copy_from_slice(request);

        let (err, _) = call(
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

        resume_caller();

        let mut recv_buf = [0u8; 128];
        let (msg_len, _reply_cap) = do_recv(ep_hid, &mut recv_buf);

        assert_eq!(msg_len, request.len());
        assert_eq!(&recv_buf[..msg_len], request);

        inv();
    }

    #[test]
    fn ipc_reply_wakes_caller() {
        let ep_hid = setup_ipc();
        let mut call_buf = [0u8; 128];

        do_call(ep_hid, b"req", &mut call_buf);
        resume_caller();

        let mut recv_buf = [0u8; 128];
        let (_msg_len, reply_cap) = do_recv(ep_hid, &mut recv_buf);

        do_reply(ep_hid, reply_cap, b"resp");

        inv();
    }

    // ── Stale handle tests ──────────────────────────────────

    #[test]
    fn stale_vmo_handle_rejected() {
        setup();

        let hid = create_stale_vmo_handle();
        let (err, _) = call(num::VMO_SEAL, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    #[test]
    fn stale_event_handle_rejected() {
        setup();

        let hid = create_stale_event_handle();
        let (err, _) = call(num::EVENT_SIGNAL, &[hid, 0b1, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    #[test]
    fn stale_endpoint_handle_rejected() {
        setup();

        let hid = create_stale_endpoint_handle();
        let (err, _) = call(num::RECV, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    #[test]
    fn stale_thread_handle_rejected() {
        setup();

        let hid = create_stale_thread_handle();
        let (err, _) = call(num::THREAD_SET_PRIORITY, &[hid, 3, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    #[test]
    fn stale_space_handle_rejected() {
        setup();

        let hid = create_stale_space_handle();
        let (err, _) = call(num::SPACE_DESTROY, &[hid, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::GenerationMismatch as u64);
    }

    // ── Rights enforcement ──────────────────────────────────

    #[test]
    fn insufficient_rights_rejected() {
        setup();

        let hid = create_vmo();
        let read_only = dup_with_rights(hid, Rights::READ.0);
        let (err, _) = call(num::VMO_SEAL, &[read_only, 0, 0, 0, 0, 0]);

        assert_eq!(err, SyscallError::InsufficientRights as u64);

        inv();
    }

    #[test]
    fn dup_without_dup_right_rejected() {
        setup();

        let hid = create_vmo();
        let no_dup = dup_with_rights(hid, Rights::READ.0);
        let (err, _) = call(
            num::HANDLE_DUP,
            &[no_dup, Rights::READ.0 as u64, 0, 0, 0, 0],
        );

        assert_eq!(err, SyscallError::InsufficientRights as u64);

        inv();
    }

    // ── IPC round-trip tests ────────────────────────────────

    #[test]
    fn ipc_ping_pong_10_rounds() {
        let ep_hid = setup_ipc();

        for round in 0..10u8 {
            let mut call_buf = [0u8; 128];
            let request = [round; 4];

            do_call(ep_hid, &request, &mut call_buf);

            let mut recv_buf = [0u8; 128];
            let (msg_len, reply_cap) = do_recv(ep_hid, &mut recv_buf);

            assert_eq!(msg_len, 4);
            assert_eq!(recv_buf[0], round);

            let response = [round.wrapping_add(100); 4];

            do_reply(ep_hid, reply_cap, &response);
            resume_caller();

            assert_eq!(call_buf[0], round.wrapping_add(100));
        }

        inv();
    }

    #[test]
    fn ipc_reply_cap_not_reusable() {
        let ep_hid = setup_ipc();
        let mut call_buf = [0u8; 128];

        do_call(ep_hid, b"req", &mut call_buf);

        let mut recv_buf = [0u8; 128];
        let (_, reply_cap) = do_recv(ep_hid, &mut recv_buf);

        do_reply(ep_hid, reply_cap, b"resp");

        let (err, _) = call(
            num::REPLY,
            &[ep_hid, reply_cap, b"dup".as_ptr() as u64, 3, 0, 0],
        );

        assert_ne!(err, 0, "double reply must fail");

        inv();
    }

    #[test]
    fn ipc_different_endpoints_independent() {
        setup();

        let ep1 = create_endpoint();
        let _ep2 = create_endpoint();
        let mut call_buf = [0u8; 128];

        do_call(ep1, b"msg1", &mut call_buf);

        let ep1_queue = state::endpoints().read(0).unwrap().pending_call_count();
        let ep2_queue = state::endpoints().read(1).unwrap().pending_call_count();

        assert_eq!(ep1_queue, 1);
        assert_eq!(ep2_queue, 0);

        let mut recv_buf = [0u8; 128];
        let (_, reply_cap) = do_recv(ep1, &mut recv_buf);

        do_reply(ep1, reply_cap, b"ack");

        inv();
    }

    #[test]
    fn ipc_handle_transfer_single() {
        let ep_hid = setup_ipc();
        let vmo_hid = create_vmo();
        let mut handles_to_send = [vmo_hid as u32];
        let msg = b"xfer";
        let mut call_buf = [0u8; 128];

        call_buf[..msg.len()].copy_from_slice(msg);

        let (err, _) = call(
            num::CALL,
            &[
                ep_hid,
                call_buf.as_mut_ptr() as u64,
                msg.len() as u64,
                handles_to_send.as_mut_ptr() as u64,
                1,
                0,
            ],
        );

        assert_eq!(err, 0, "CALL with handle transfer failed");
        assert_err(
            call(num::HANDLE_INFO, &[vmo_hid, 0, 0, 0, 0, 0]),
            SyscallError::InvalidHandle,
        );

        let mut recv_buf = [0u8; 128];
        let mut handles_out = [0u32; 4];
        let mut reply_cap: u64 = 0;
        let (err, packed) = call(
            num::RECV,
            &[
                ep_hid,
                recv_buf.as_mut_ptr() as u64,
                128,
                handles_out.as_mut_ptr() as u64,
                4,
                &raw mut reply_cap as u64,
            ],
        );

        assert_eq!(err, 0);

        let h_count = (packed >> 16) & 0xFFFF;

        assert_eq!(h_count, 1, "server should receive 1 handle");
        assert_ne!(handles_out[0], 0, "transferred handle should be valid");

        let (err, info) = call(num::HANDLE_INFO, &[handles_out[0] as u64, 0, 0, 0, 0, 0]);

        assert_eq!(err, 0);
        assert_eq!(
            (info >> 32) as u8,
            ObjectType::Vmo as u8,
            "transferred handle should be a VMO"
        );

        do_reply(ep_hid, reply_cap, b"ok");

        inv();
    }

    // ── Endpoint bind-event tests ───────────────────────────

    fn lookup_obj_id(hid: u64) -> u32 {
        state::spaces()
            .read(0)
            .unwrap()
            .handles()
            .lookup(HandleId(hid as u32))
            .unwrap()
            .object_id
    }

    #[test]
    fn endpoint_bind_event_signals_on_enqueue() {
        setup();

        let ep_hid = create_endpoint();
        let event_hid = create_event();
        let (err, _) = call(num::ENDPOINT_BIND_EVENT, &[ep_hid, event_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let event_obj = lookup_obj_id(event_hid);

        assert_eq!(state::events().read(event_obj).unwrap().bits(), 0);

        let mut call_buf = [0u8; 128];

        do_call(ep_hid, b"hello", &mut call_buf);

        let bits = state::events().read(event_obj).unwrap().bits();

        assert_ne!(bits, 0, "event should be signaled after enqueue");
        assert_eq!(
            bits,
            crate::endpoint::Endpoint::ENDPOINT_READABLE_BIT,
            "should signal ENDPOINT_READABLE_BIT"
        );

        let mut recv_buf = [0u8; 128];
        let (_, reply_cap) = do_recv(ep_hid, &mut recv_buf);

        do_reply(ep_hid, reply_cap, b"ok");

        inv();
    }

    #[test]
    fn endpoint_no_signal_without_binding() {
        setup();

        let ep_hid = create_endpoint();
        let event_hid = create_event();
        let event_obj = lookup_obj_id(event_hid);
        let mut call_buf = [0u8; 128];

        do_call(ep_hid, b"hello", &mut call_buf);

        assert_eq!(
            state::events().read(event_obj).unwrap().bits(),
            0,
            "event should NOT be signaled without binding"
        );

        let mut recv_buf = [0u8; 128];
        let (_, reply_cap) = do_recv(ep_hid, &mut recv_buf);

        do_reply(ep_hid, reply_cap, b"ok");

        inv();
    }

    #[test]
    fn endpoint_bind_event_with_pending_signals_immediately() {
        setup();

        let ep_hid = create_endpoint();
        let event_hid = create_event();
        let caller_hid = create_thread();
        let caller_obj = lookup_obj_id(caller_hid);
        let caller_tid = ThreadId(caller_obj);

        {
            let picked = state::schedulers().core(0).lock().pick_next();

            assert_eq!(picked, Some(caller_tid));
        }

        state::threads()
            .write(caller_obj)
            .unwrap()
            .set_state(ThreadRunState::Running);

        let mut call_buf = [0u8; 128];

        call_buf[..5].copy_from_slice(b"hello");

        let (err, _) = call_as(
            caller_tid,
            num::CALL,
            &[ep_hid, call_buf.as_mut_ptr() as u64, 5, 0, 0, 0],
        );

        assert_eq!(err, 0);

        state::schedulers()
            .core(0)
            .lock()
            .set_current(Some(ThreadId(0)));

        let event_obj = lookup_obj_id(event_hid);

        assert_eq!(state::events().read(event_obj).unwrap().bits(), 0);

        let (err, _) = call(num::ENDPOINT_BIND_EVENT, &[ep_hid, event_hid, 0, 0, 0, 0]);

        assert_eq!(err, 0);

        let bits = state::events().read(event_obj).unwrap().bits();

        assert_ne!(
            bits, 0,
            "binding to endpoint with pending calls should signal immediately"
        );

        inv();
    }

    // ── Clock tests ─────────────────────────────────────────

    #[test]
    fn clock_read_returns_value() {
        setup();

        let (err, val) = call(num::CLOCK_READ, &[0; 6]);

        assert_eq!(err, 0);
        assert!(val > 0, "clock should return nonzero nanoseconds");

        inv();
    }

    #[test]
    fn clock_read_advances() {
        setup();

        let (_, t1) = call(num::CLOCK_READ, &[0; 6]);

        for _ in 0..1000 {
            core::hint::spin_loop();
        }

        let (_, t2) = call(num::CLOCK_READ, &[0; 6]);

        assert!(t2 >= t1, "clock must not go backwards");

        inv();
    }
}
