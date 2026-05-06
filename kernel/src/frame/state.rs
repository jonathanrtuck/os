//! Global kernel state — per-object-type ConcurrentTables, scheduler, IRQs.
//!
//! Initialized once during single-threaded boot via `init()`. After init,
//! the accessor functions return `&'static` references to internally-
//! synchronized tables. Each syscall handler acquires only the locks it
//! needs from these tables — no global kernel lock.

use core::{
    ptr::addr_of_mut,
    sync::atomic::{AtomicU32, Ordering},
};

use super::{
    arch::sync::SpinLock,
    concurrent_table::ConcurrentTable,
    slab::{BoxStorage, InlineSlab},
};
use crate::{
    address_space::AddressSpace, config, endpoint::Endpoint, event::Event, irq::IrqTable,
    table::ObjectTable, thread::PerCoreState, types::ThreadId, vmo::Vmo,
};

pub type VmoTable = ConcurrentTable<Vmo, { config::MAX_VMOS }, InlineSlab<Vmo>>;
pub type EventTable = ConcurrentTable<Event, { config::MAX_EVENTS }, InlineSlab<Event>>;
pub type EndpointTable = ConcurrentTable<Endpoint, { config::MAX_ENDPOINTS }, InlineSlab<Endpoint>>;
pub type ThreadTable = ConcurrentTable<
    crate::thread::Thread,
    { config::MAX_THREADS },
    BoxStorage<crate::thread::Thread>,
>;
pub type SpaceTable =
    ConcurrentTable<AddressSpace, { config::MAX_ADDRESS_SPACES }, BoxStorage<AddressSpace>>;

/// Per-CPU scheduler array — each core's `PerCoreState` behind its own
/// `SpinLock`. Independent cores never contend; cross-core wake contends
/// only with the target core.
pub struct Schedulers {
    cores: alloc::vec::Vec<SpinLock<PerCoreState>>,
}

impl Schedulers {
    pub fn new(num_cores: usize) -> Self {
        let mut cores = alloc::vec::Vec::with_capacity(num_cores);

        for _ in 0..num_cores {
            cores.push(SpinLock::new(PerCoreState::new()));
        }

        Schedulers { cores }
    }

    pub fn core(&self, core_id: usize) -> &SpinLock<PerCoreState> {
        &self.cores[core_id]
    }

    pub fn num_cores(&self) -> usize {
        self.cores.len()
    }

    pub fn remove(&self, thread: ThreadId) {
        for core in &self.cores {
            if core.lock().remove_if_present(thread) {
                return;
            }
        }
    }

    pub fn least_loaded_core(&self) -> usize {
        self.cores
            .iter()
            .enumerate()
            .min_by_key(|(_, core)| core.lock().total_ready())
            .map_or(0, |(i, _)| i)
    }
}

static mut VMOS: Option<VmoTable> = None;
static mut EVENTS: Option<EventTable> = None;
static mut ENDPOINTS: Option<EndpointTable> = None;
static mut THREADS: Option<ThreadTable> = None;
static mut SPACES: Option<SpaceTable> = None;
static mut SCHEDULERS: Option<Schedulers> = None;
static mut IRQS: Option<SpinLock<IrqTable>> = None;
static ALIVE_THREADS: AtomicU32 = AtomicU32::new(0);

/// Initialize all global kernel state. Must be called exactly once during
/// single-threaded boot, before any thread runs.
pub fn init(num_cores: usize) {
    // SAFETY: called once during single-core boot. After this, only the
    // safe accessor functions are used, which return &'static references
    // to internally-synchronized tables. We use addr_of_mut! to avoid
    // creating references to the static mut (Rust 2024 requirement).
    unsafe {
        addr_of_mut!(VMOS).write(Some(ConcurrentTable::from_table(ObjectTable::new())));
        addr_of_mut!(EVENTS).write(Some(ConcurrentTable::from_table(ObjectTable::new())));
        addr_of_mut!(ENDPOINTS).write(Some(ConcurrentTable::from_table(ObjectTable::new())));
        addr_of_mut!(THREADS).write(Some(ConcurrentTable::from_table(ObjectTable::new())));
        addr_of_mut!(SPACES).write(Some(ConcurrentTable::from_table(ObjectTable::new())));
        addr_of_mut!(SCHEDULERS).write(Some(Schedulers::new(num_cores)));
        addr_of_mut!(IRQS).write(Some(SpinLock::new(IrqTable::new())));
        ALIVE_THREADS.store(0, Ordering::Relaxed);
    }
}

pub fn vmos() -> &'static VmoTable {
    // SAFETY: init() was called during boot. VMOS is Some and never
    // reassigned after init. addr_of_mut avoids creating a reference
    // to the static mut itself.
    unsafe { (*addr_of_mut!(VMOS)).as_ref().unwrap_unchecked() }
}

pub fn events() -> &'static EventTable {
    unsafe { (*addr_of_mut!(EVENTS)).as_ref().unwrap_unchecked() }
}

pub fn endpoints() -> &'static EndpointTable {
    unsafe { (*addr_of_mut!(ENDPOINTS)).as_ref().unwrap_unchecked() }
}

pub fn threads() -> &'static ThreadTable {
    unsafe { (*addr_of_mut!(THREADS)).as_ref().unwrap_unchecked() }
}

pub fn spaces() -> &'static SpaceTable {
    unsafe { (*addr_of_mut!(SPACES)).as_ref().unwrap_unchecked() }
}

pub fn schedulers() -> &'static Schedulers {
    unsafe { (*addr_of_mut!(SCHEDULERS)).as_ref().unwrap_unchecked() }
}

pub fn irqs() -> &'static SpinLock<IrqTable> {
    unsafe { (*addr_of_mut!(IRQS)).as_ref().unwrap_unchecked() }
}

pub fn alive_threads() -> &'static AtomicU32 {
    &ALIVE_THREADS
}

pub fn inc_alive_threads() {
    ALIVE_THREADS.fetch_add(1, Ordering::Relaxed);
}

pub fn dec_alive_threads() -> u32 {
    ALIVE_THREADS.fetch_sub(1, Ordering::Relaxed) - 1
}

pub fn alive_thread_count() -> u32 {
    ALIVE_THREADS.load(Ordering::Relaxed)
}

#[cfg(any(target_os = "none", test))]
pub fn alloc_asid() -> Result<u8, crate::types::SyscallError> {
    crate::frame::arch::page_table::alloc_asid()
        .map(|asid| asid.0)
        .ok_or(crate::types::SyscallError::OutOfMemory)
}

#[cfg(not(any(target_os = "none", test)))]
pub fn alloc_asid() -> Result<u8, crate::types::SyscallError> {
    Err(crate::types::SyscallError::OutOfMemory)
}

#[cfg(any(target_os = "none", test))]
pub fn free_asid(asid: u8) {
    if asid != 0 {
        crate::frame::arch::page_table::free_asid(crate::frame::arch::page_table::Asid(asid));
    }
}

#[cfg(not(any(target_os = "none", test)))]
pub fn free_asid(_asid: u8) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_accessors() {
        init(1);

        // Accessors return valid references (don't panic).
        let _ = vmos().count();
        let _ = events().count();
        let _ = endpoints().count();
        let _ = threads().count();
        let _ = spaces().count();
        let _ = alive_thread_count();

        assert_eq!(schedulers().num_cores(), 1);
    }

    #[test]
    fn alloc_through_global_vmos() {
        init(1);

        let vmo = crate::vmo::Vmo::new(crate::types::VmoId(0), 4096, crate::vmo::VmoFlags::NONE);
        let (idx, generation) = vmos().alloc_shared(vmo).unwrap();

        assert_eq!(generation, 0);

        let guard = vmos().read(idx).unwrap();

        assert_eq!(guard.size(), 4096);
    }

    #[test]
    fn alive_thread_counter() {
        init(1);

        assert_eq!(alive_thread_count(), 0);

        inc_alive_threads();
        inc_alive_threads();

        assert_eq!(alive_thread_count(), 2);

        let remaining = dec_alive_threads();

        assert_eq!(remaining, 1);
    }
}
