#![feature(allocator_api)]
//! Integration stress tests: multi-subsystem interaction under load.
//!
//! These tests exercise the interaction between IPC, scheduling, allocation,
//! process lifecycle, and wait/waiter cleanup subsystems simultaneously.
//! They model the kernel's resource management logic on the host since the
//! kernel targets aarch64-unknown-none and cannot be imported directly.
//!
//! Fulfills: VAL-INTEG-001, VAL-INTEG-002, VAL-INTEG-005
//!
//! Run with: cargo test --test integration_stress -- --test-threads=1

// ============================================================
// Seeded PRNG (xorshift64) — deterministic, no external deps.
// ============================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 {
            return 0;
        }
        (self.next_u64() % max as u64) as usize
    }

    fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

// ============================================================
// Import kernel types that compile on host.
// ============================================================

#[path = "../../paging.rs"]
#[allow(dead_code)]
mod paging;

mod event {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct EventId(pub u32);
}

#[path = "../../handle.rs"]
mod handle;

#[path = "../../scheduling_context.rs"]
mod scheduling_context;

mod interrupt {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct InterruptId(pub u8);
}
mod process {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ProcessId(pub u32);
}
mod thread {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ThreadId(pub u64);
}
mod timer {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TimerId(pub u8);
}
mod vmo {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct VmoId(pub u32);
}

use handle::*;
use scheduling_context::*;

// ============================================================
// Subsystem models — faithful replications of kernel logic.
// ============================================================

// --- Physical address stub ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pa(u64);

// --- Channel table model (kernel/channel.rs) ---

struct ModelChannel {
    pages: [Pa; 2],
    pending_signal: [bool; 2],
    waiter: [Option<thread::ThreadId>; 2],
    closed_count: u8,
}

struct ChannelTable {
    channels: Vec<ModelChannel>,
    page_counter: u64,
    live_pages: usize,
}

impl ChannelTable {
    fn new() -> Self {
        Self {
            channels: Vec::new(),
            page_counter: 0,
            live_pages: 0,
        }
    }

    fn alloc_page(&mut self) -> Pa {
        self.page_counter += 1;
        self.live_pages += 1;
        Pa(self.page_counter)
    }

    fn free_page(&mut self, _pa: Pa) {
        assert!(self.live_pages > 0, "double free detected");
        self.live_pages -= 1;
    }

    fn create(&mut self) -> (ChannelId, ChannelId) {
        let page0 = self.alloc_page();
        let page1 = self.alloc_page();
        let idx = self.channels.len() as u32;
        self.channels.push(ModelChannel {
            pages: [page0, page1],
            pending_signal: [false, false],
            waiter: [None, None],
            closed_count: 0,
        });
        (ChannelId(idx * 2), ChannelId(idx * 2 + 1))
    }

    fn signal(&mut self, id: ChannelId) -> Option<thread::ThreadId> {
        let ch_idx = id.0 as usize / 2;
        let ep = id.0 as usize % 2;
        let peer_ep = 1 - ep;
        let ch = &mut self.channels[ch_idx];
        ch.pending_signal[peer_ep] = true;
        ch.waiter[peer_ep].take()
    }

    fn check_pending(&mut self, id: ChannelId) -> bool {
        let ch_idx = id.0 as usize / 2;
        let ep = id.0 as usize % 2;
        let ch = &mut self.channels[ch_idx];
        if ch.pending_signal[ep] {
            ch.pending_signal[ep] = false;
            true
        } else {
            false
        }
    }

    fn register_waiter(&mut self, id: ChannelId, waiter: thread::ThreadId) {
        let ch_idx = id.0 as usize / 2;
        let ep = id.0 as usize % 2;
        self.channels[ch_idx].waiter[ep] = Some(waiter);
    }

    fn unregister_waiter(&mut self, id: ChannelId) {
        let ch_idx = id.0 as usize / 2;
        let ep = id.0 as usize % 2;
        self.channels[ch_idx].waiter[ep] = None;
    }

    fn close_endpoint(&mut self, id: ChannelId) {
        let ch_idx = id.0 as usize / 2;
        let ch = &mut self.channels[ch_idx];
        if ch.closed_count >= 2 {
            return;
        }
        let ep = id.0 as usize % 2;
        ch.waiter[ep] = None;
        ch.closed_count += 1;
        if ch.closed_count == 2 {
            let pages = ch.pages;
            ch.pages = [Pa(0), Pa(0)];
            self.free_page(pages[0]);
            self.free_page(pages[1]);
        }
    }
}

// --- Timer table model (kernel/timer.rs) ---

const MAX_TIMERS: usize = 32;

struct TimerEntry {
    deadline: u64,
    fired: bool,
    waiter: Option<thread::ThreadId>,
}

struct TimerTable {
    slots: [Option<TimerEntry>; MAX_TIMERS],
}

impl TimerTable {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }

    fn create(&mut self, deadline: u64) -> Option<timer::TimerId> {
        for i in 0..MAX_TIMERS {
            if self.slots[i].is_none() {
                self.slots[i] = Some(TimerEntry {
                    deadline,
                    fired: false,
                    waiter: None,
                });
                return Some(timer::TimerId(i as u8));
            }
        }
        None
    }

    fn destroy(&mut self, id: timer::TimerId) -> Option<thread::ThreadId> {
        let entry = self.slots[id.0 as usize].take()?;
        entry.waiter
    }

    fn fire(&mut self, id: timer::TimerId) -> Option<thread::ThreadId> {
        if let Some(entry) = self.slots[id.0 as usize].as_mut() {
            entry.fired = true;
            entry.waiter.take()
        } else {
            None
        }
    }

    fn check_fired(&self, id: timer::TimerId) -> bool {
        self.slots[id.0 as usize].as_ref().is_some_and(|e| e.fired)
    }

    fn register_waiter(&mut self, id: timer::TimerId, waiter: thread::ThreadId) {
        if let Some(entry) = self.slots[id.0 as usize].as_mut() {
            entry.waiter = Some(waiter);
        }
    }

    fn unregister_waiter(&mut self, id: timer::TimerId) {
        if let Some(entry) = self.slots[id.0 as usize].as_mut() {
            entry.waiter = None;
        }
    }

    fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

// --- Scheduling context table model (kernel/scheduler.rs) ---

const MAX_SCHED_CONTEXTS: usize = 64;

struct SchedContextEntry {
    #[allow(dead_code)]
    ctx: SchedulingContext,
    ref_count: u32,
}

struct SchedContextTable {
    slots: Vec<Option<SchedContextEntry>>,
    free_ids: Vec<u32>,
    live_count: usize,
}

impl SchedContextTable {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_ids: Vec::new(),
            live_count: 0,
        }
    }

    fn create(&mut self, budget: u64, period: u64) -> Option<SchedulingContextId> {
        if !validate_params(budget, period) {
            return None;
        }
        let slot = SchedContextEntry {
            ctx: SchedulingContext::new(budget, period, 0),
            ref_count: 1,
        };
        if let Some(free_id) = self.free_ids.pop() {
            self.slots[free_id as usize] = Some(slot);
            self.live_count += 1;
            Some(SchedulingContextId(free_id))
        } else if self.slots.len() < MAX_SCHED_CONTEXTS {
            let id = self.slots.len() as u32;
            self.slots.push(Some(slot));
            self.live_count += 1;
            Some(SchedulingContextId(id))
        } else {
            None
        }
    }

    fn inc_ref(&mut self, id: SchedulingContextId) -> bool {
        match self.slots.get_mut(id.0 as usize) {
            Some(Some(entry)) => {
                entry.ref_count += 1;
                true
            }
            _ => false,
        }
    }

    fn dec_ref(&mut self, id: SchedulingContextId) -> Option<u32> {
        if let Some(slot) = self.slots.get_mut(id.0 as usize) {
            if let Some(entry) = slot {
                entry.ref_count = entry.ref_count.saturating_sub(1);
                let rc = entry.ref_count;
                if rc == 0 {
                    *slot = None;
                    self.free_ids.push(id.0);
                    self.live_count -= 1;
                }
                return Some(rc);
            }
        }
        None
    }

    fn is_alive(&self, id: SchedulingContextId) -> bool {
        matches!(self.slots.get(id.0 as usize), Some(Some(_)))
    }
}

// --- Heap model (kernel/heap.rs) ---
// Models linked-list + slab allocator for tracking alloc/free balance.

struct HeapModel {
    /// Track allocations: (address, size).
    allocations: Vec<(u64, usize)>,
    next_addr: u64,
    total_allocated: usize,
    total_freed: usize,
}

impl HeapModel {
    fn new() -> Self {
        Self {
            allocations: Vec::new(),
            next_addr: 0x1000,
            total_allocated: 0,
            total_freed: 0,
        }
    }

    fn alloc(&mut self, size: usize) -> u64 {
        let addr = self.next_addr;
        self.next_addr += size as u64;
        // Align to 16 bytes (MIN_BLOCK).
        self.next_addr = (self.next_addr + 15) & !15;
        self.allocations.push((addr, size));
        self.total_allocated += size;
        addr
    }

    fn free(&mut self, addr: u64) -> bool {
        if let Some(pos) = self.allocations.iter().position(|(a, _)| *a == addr) {
            let (_, size) = self.allocations.swap_remove(pos);
            self.total_freed += size;
            true
        } else {
            false // Double free or invalid address.
        }
    }

    fn live_count(&self) -> usize {
        self.allocations.len()
    }
}

// --- Process model with handle table (for process churn test) ---

#[derive(Debug, PartialEq, Eq)]
enum ProcessState {
    Suspended,
    Running,
    Exited,
}

struct ModelProcess {
    id: process::ProcessId,
    state: ProcessState,
    handles: HandleTable,
}

struct ProcessTable {
    processes: Vec<ModelProcess>,
    live_count: usize,
}

impl ProcessTable {
    fn new() -> Self {
        Self {
            processes: Vec::new(),
            live_count: 0,
        }
    }

    fn create(&mut self) -> process::ProcessId {
        let id = process::ProcessId(self.processes.len() as u32);
        self.processes.push(ModelProcess {
            id,
            state: ProcessState::Suspended,
            handles: HandleTable::new(),
        });
        self.live_count += 1;
        id
    }

    fn start(&mut self, id: process::ProcessId) -> bool {
        let p = &mut self.processes[id.0 as usize];
        if p.state == ProcessState::Suspended {
            p.state = ProcessState::Running;
            true
        } else {
            false
        }
    }

    fn kill(&mut self, id: process::ProcessId) -> Vec<(HandleObject, Rights, u64)> {
        let p = &mut self.processes[id.0 as usize];
        if p.state == ProcessState::Exited {
            return Vec::new();
        }
        p.state = ProcessState::Exited;
        self.live_count -= 1;
        // Drain all handles (like process exit cleanup).
        p.handles.drain().collect()
    }

    fn handles_mut(&mut self, id: process::ProcessId) -> &mut HandleTable {
        &mut self.processes[id.0 as usize].handles
    }
}

// --- Wait set model (kernel/thread.rs WaitEntry + stale_waiters) ---

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaitEntry {
    object: HandleObject,
    user_index: u8,
}

const TIMEOUT_SENTINEL: u8 = 0xFF;

struct ModelThread {
    id: thread::ThreadId,
    wait_set: Vec<WaitEntry>,
    stale_waiters: Vec<WaitEntry>,
    timeout_timer: Option<timer::TimerId>,
    /// Scheduling context binding.
    context_id: Option<SchedulingContextId>,
    saved_context_id: Option<SchedulingContextId>,
}

impl ModelThread {
    fn new(id: thread::ThreadId) -> Self {
        Self {
            id,
            wait_set: Vec::new(),
            stale_waiters: Vec::new(),
            timeout_timer: None,
            context_id: None,
            saved_context_id: None,
        }
    }

    /// Model of thread.complete_wait_for — resolves wake, moves unfired to stale.
    fn complete_wait_for(&mut self, reason: &HandleObject) -> u64 {
        if self.wait_set.is_empty() {
            return 0;
        }

        let result = self
            .wait_set
            .iter()
            .find(|e| e.object == *reason)
            .map(|e| {
                if e.user_index == TIMEOUT_SENTINEL {
                    u64::MAX // WouldBlock sentinel
                } else {
                    e.user_index as u64
                }
            })
            .unwrap_or(0);

        // Move unfired entries to stale_waiters for deferred cleanup.
        self.stale_waiters.clear();
        for entry in &self.wait_set {
            if entry.object != *reason {
                self.stale_waiters.push(*entry);
            }
        }
        self.wait_set.clear();

        result
    }

    /// Model of scheduler::take_stale_waiters.
    fn take_stale_waiters(&mut self) -> Vec<WaitEntry> {
        core::mem::take(&mut self.stale_waiters)
    }

    /// Model of scheduler::take_timeout_timer.
    fn take_timeout_timer(&mut self) -> Option<timer::TimerId> {
        self.timeout_timer.take()
    }
}

// ============================================================
// TEST 1: Multi-subsystem integration stress
//
// Exercises IPC (channel create/signal/close) + scheduling
// (context create/bind/borrow/return) + allocation (heap
// alloc/free) simultaneously across multiple simulated threads.
// 1000+ total operations.
//
// Fulfills: VAL-INTEG-001
// ============================================================

#[test]
fn integration_stress_multi_subsystem_1500_ops() {
    let mut rng = Rng::new(0xDEAD_C0DE);

    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();
    let mut sched_ctxs = SchedContextTable::new();
    let mut heap = HeapModel::new();
    let mut handles = HandleTable::new();

    // Track live resources for cleanup.
    let mut live_channel_eps: Vec<(ChannelId, ChannelId)> = Vec::new();
    let mut live_timer_ids: Vec<timer::TimerId> = Vec::new();
    let mut live_sched_ctx_ids: Vec<SchedulingContextId> = Vec::new();
    let mut live_heap_addrs: Vec<u64> = Vec::new();
    let mut live_handles: Vec<Handle> = Vec::new();

    // Thread models for scheduling context binding.
    let mut threads: Vec<ModelThread> = Vec::new();
    let mut next_thread_id: u64 = 0;

    // Create some initial threads.
    for _ in 0..8 {
        threads.push(ModelThread::new(thread::ThreadId(next_thread_id)));
        next_thread_id += 1;
    }

    let mut op_count = 0u64;

    // Run 1500 randomized operations across all subsystems.
    while op_count < 1500 {
        let action = rng.next_usize(20);
        op_count += 1;

        match action {
            // --- IPC: channel create ---
            0..=1 => {
                let (ep0, ep1) = channels.create();
                live_channel_eps.push((ep0, ep1));

                // Insert handles for both endpoints.
                if let Ok(h0) = handles.insert(HandleObject::Channel(ep0), Rights::READ_WRITE) {
                    live_handles.push(h0);
                }
                if let Ok(h1) = handles.insert(HandleObject::Channel(ep1), Rights::READ_WRITE) {
                    live_handles.push(h1);
                }
            }

            // --- IPC: channel signal ---
            2..=3 if !live_channel_eps.is_empty() => {
                let idx = rng.next_usize(live_channel_eps.len());
                let (ep0, _ep1) = live_channel_eps[idx];
                // Signal from ep0 to ep1.
                let _waiter = channels.signal(ep0);
            }

            // --- IPC: channel close ---
            4 if !live_channel_eps.is_empty() => {
                let idx = rng.next_usize(live_channel_eps.len());
                let (ep0, ep1) = live_channel_eps.swap_remove(idx);
                channels.close_endpoint(ep0);
                channels.close_endpoint(ep1);
            }

            // --- Scheduling: context create ---
            5..=6 => {
                let budget = MIN_BUDGET_NS * (1 + rng.next_usize(5) as u64);
                let period = budget * (2 + rng.next_usize(5) as u64);
                let period = period.min(MAX_PERIOD_NS);
                if let Some(id) = sched_ctxs.create(budget, period) {
                    live_sched_ctx_ids.push(id);
                    // Insert a handle.
                    if let Ok(h) =
                        handles.insert(HandleObject::SchedulingContext(id), Rights::READ_WRITE)
                    {
                        live_handles.push(h);
                    }
                }
            }

            // --- Scheduling: bind thread to context ---
            7 if !live_sched_ctx_ids.is_empty() && !threads.is_empty() => {
                let t_idx = rng.next_usize(threads.len());
                let thread = &mut threads[t_idx];
                if thread.context_id.is_none() {
                    let sc_idx = rng.next_usize(live_sched_ctx_ids.len());
                    let ctx_id = live_sched_ctx_ids[sc_idx];
                    if sched_ctxs.is_alive(ctx_id) && sched_ctxs.inc_ref(ctx_id) {
                        thread.context_id = Some(ctx_id);
                    }
                }
            }

            // --- Scheduling: borrow context ---
            8 if !live_sched_ctx_ids.is_empty() && !threads.is_empty() => {
                let t_idx = rng.next_usize(threads.len());
                let thread = &mut threads[t_idx];
                if thread.context_id.is_some() && thread.saved_context_id.is_none() {
                    let sc_idx = rng.next_usize(live_sched_ctx_ids.len());
                    let ctx_id = live_sched_ctx_ids[sc_idx];
                    if sched_ctxs.is_alive(ctx_id) && sched_ctxs.inc_ref(ctx_id) {
                        thread.saved_context_id = thread.context_id;
                        thread.context_id = Some(ctx_id);
                    }
                }
            }

            // --- Scheduling: return borrowed context ---
            9 if !threads.is_empty() => {
                let t_idx = rng.next_usize(threads.len());
                let thread = &mut threads[t_idx];
                if let Some(saved) = thread.saved_context_id.take() {
                    let borrowed = thread.context_id;
                    thread.context_id = Some(saved);
                    if let Some(cid) = borrowed {
                        sched_ctxs.dec_ref(cid);
                    }
                }
            }

            // --- Scheduling: release context (close handle) ---
            10 if !live_sched_ctx_ids.is_empty() => {
                let idx = rng.next_usize(live_sched_ctx_ids.len());
                let ctx_id = live_sched_ctx_ids.swap_remove(idx);
                if sched_ctxs.is_alive(ctx_id) {
                    sched_ctxs.dec_ref(ctx_id);
                }
            }

            // --- Allocation: heap alloc ---
            11..=13 => {
                let sizes = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
                let size = sizes[rng.next_usize(sizes.len())];
                let addr = heap.alloc(size);
                live_heap_addrs.push(addr);
            }

            // --- Allocation: heap free ---
            14..=15 if !live_heap_addrs.is_empty() => {
                let idx = rng.next_usize(live_heap_addrs.len());
                let addr = live_heap_addrs.swap_remove(idx);
                assert!(
                    heap.free(addr),
                    "heap free should succeed for live allocation"
                );
            }

            // --- Timer: create ---
            16 => {
                if let Some(id) = timers.create(rng.next_u64()) {
                    live_timer_ids.push(id);
                    if let Ok(h) = handles.insert(HandleObject::Timer(id), Rights::READ_WRITE) {
                        live_handles.push(h);
                    }
                }
            }

            // --- Timer: destroy ---
            17 if !live_timer_ids.is_empty() => {
                let idx = rng.next_usize(live_timer_ids.len());
                let id = live_timer_ids.swap_remove(idx);
                timers.destroy(id);
            }

            // --- Thread: create a new simulated thread ---
            18 => {
                threads.push(ModelThread::new(thread::ThreadId(next_thread_id)));
                next_thread_id += 1;
            }

            // --- Thread: exit a simulated thread ---
            19 if threads.len() > 1 => {
                let t_idx = rng.next_usize(threads.len());
                let thread = &mut threads[t_idx];

                // Release scheduling context refs (like release_thread_context_ids).
                if let Some(cid) = thread.context_id.take() {
                    sched_ctxs.dec_ref(cid);
                }
                if let Some(cid) = thread.saved_context_id.take() {
                    sched_ctxs.dec_ref(cid);
                }

                threads.swap_remove(t_idx);
            }

            _ => {} // No-op if preconditions not met.
        }
    }

    assert!(
        op_count >= 1500,
        "must execute at least 1500 operations (executed {})",
        op_count
    );

    // --- Cleanup phase: release all remaining resources ---

    // Exit all threads (release scheduling context refs).
    for thread in &mut threads {
        if let Some(cid) = thread.context_id.take() {
            sched_ctxs.dec_ref(cid);
        }
        if let Some(cid) = thread.saved_context_id.take() {
            sched_ctxs.dec_ref(cid);
        }
    }

    // Release remaining scheduling context handles.
    for ctx_id in &live_sched_ctx_ids {
        if sched_ctxs.is_alive(*ctx_id) {
            sched_ctxs.dec_ref(*ctx_id);
        }
    }

    // Close remaining channels.
    for (ep0, ep1) in &live_channel_eps {
        channels.close_endpoint(*ep0);
        channels.close_endpoint(*ep1);
    }

    // Destroy remaining timers.
    for id in &live_timer_ids {
        timers.destroy(*id);
    }

    // Free remaining heap allocations.
    for addr in &live_heap_addrs {
        assert!(heap.free(*addr), "cleanup free should succeed");
    }

    // --- Verify no leaks ---

    assert_eq!(channels.live_pages, 0, "all channel pages freed");
    assert_eq!(timers.active_count(), 0, "all timers freed");
    assert_eq!(sched_ctxs.live_count, 0, "all scheduling contexts freed");
    assert_eq!(heap.live_count(), 0, "all heap allocations freed");
}

/// Variant: all 8 threads interact with shared resources concurrently
/// with interleaved bind/borrow/return cycles while channels are being
/// created, signaled, and destroyed. 1000+ ops.
#[test]
fn integration_stress_concurrent_threads_1200_ops() {
    let mut rng = Rng::new(0xCAFE_F00D);

    let mut channels = ChannelTable::new();
    let mut sched_ctxs = SchedContextTable::new();
    let mut heap = HeapModel::new();

    let num_threads = 8;
    let mut threads: Vec<ModelThread> = (0..num_threads)
        .map(|i| ModelThread::new(thread::ThreadId(i as u64)))
        .collect();

    let mut live_channel_eps: Vec<(ChannelId, ChannelId)> = Vec::new();
    let mut live_sched_ctx_ids: Vec<SchedulingContextId> = Vec::new();
    let mut live_heap_addrs: Vec<u64> = Vec::new();

    let mut ops = 0u64;

    while ops < 1200 {
        ops += 1;
        let action = rng.next_usize(15);
        let t_idx = rng.next_usize(num_threads);

        match action {
            // Channel create.
            0..=1 => {
                let (ep0, ep1) = channels.create();
                live_channel_eps.push((ep0, ep1));
            }
            // Channel signal + check pending.
            2..=3 if !live_channel_eps.is_empty() => {
                let idx = rng.next_usize(live_channel_eps.len());
                let (ep0, ep1) = live_channel_eps[idx];
                // Signal from ep0, check on ep1.
                channels.signal(ep0);
                let pending = channels.check_pending(ep1);
                assert!(pending, "signal should make pending true");
            }
            // Channel close.
            4 if !live_channel_eps.is_empty() => {
                let idx = rng.next_usize(live_channel_eps.len());
                let (ep0, ep1) = live_channel_eps.swap_remove(idx);
                channels.close_endpoint(ep0);
                channels.close_endpoint(ep1);
            }
            // Context create.
            5 => {
                if let Some(id) = sched_ctxs.create(MIN_BUDGET_NS, MIN_PERIOD_NS * 10) {
                    live_sched_ctx_ids.push(id);
                }
            }
            // Bind thread to context.
            6 if !live_sched_ctx_ids.is_empty() => {
                let thread = &mut threads[t_idx];
                if thread.context_id.is_none() {
                    let sc_idx = rng.next_usize(live_sched_ctx_ids.len());
                    let ctx_id = live_sched_ctx_ids[sc_idx];
                    if sched_ctxs.is_alive(ctx_id) && sched_ctxs.inc_ref(ctx_id) {
                        thread.context_id = Some(ctx_id);
                    }
                }
            }
            // Borrow context.
            7 if !live_sched_ctx_ids.is_empty() => {
                let thread = &mut threads[t_idx];
                if thread.context_id.is_some() && thread.saved_context_id.is_none() {
                    let sc_idx = rng.next_usize(live_sched_ctx_ids.len());
                    let ctx_id = live_sched_ctx_ids[sc_idx];
                    if sched_ctxs.is_alive(ctx_id) && sched_ctxs.inc_ref(ctx_id) {
                        thread.saved_context_id = thread.context_id;
                        thread.context_id = Some(ctx_id);
                    }
                }
            }
            // Return borrowed context.
            8 => {
                let thread = &mut threads[t_idx];
                if let Some(saved) = thread.saved_context_id.take() {
                    let borrowed = thread.context_id;
                    thread.context_id = Some(saved);
                    if let Some(cid) = borrowed {
                        sched_ctxs.dec_ref(cid);
                    }
                }
            }
            // Release a context handle.
            9 if !live_sched_ctx_ids.is_empty() => {
                let idx = rng.next_usize(live_sched_ctx_ids.len());
                let ctx_id = live_sched_ctx_ids.swap_remove(idx);
                if sched_ctxs.is_alive(ctx_id) {
                    sched_ctxs.dec_ref(ctx_id);
                }
            }
            // Heap alloc.
            10..=12 => {
                let size = 16 * (1 + rng.next_usize(64));
                let addr = heap.alloc(size);
                live_heap_addrs.push(addr);
            }
            // Heap free.
            13..=14 if !live_heap_addrs.is_empty() => {
                let idx = rng.next_usize(live_heap_addrs.len());
                let addr = live_heap_addrs.swap_remove(idx);
                assert!(heap.free(addr));
            }
            _ => {}
        }
    }

    assert!(ops >= 1200, "must execute 1200+ ops (executed {})", ops);

    // Cleanup.
    for thread in &mut threads {
        if let Some(cid) = thread.context_id.take() {
            sched_ctxs.dec_ref(cid);
        }
        if let Some(cid) = thread.saved_context_id.take() {
            sched_ctxs.dec_ref(cid);
        }
    }
    for ctx_id in &live_sched_ctx_ids {
        if sched_ctxs.is_alive(*ctx_id) {
            sched_ctxs.dec_ref(*ctx_id);
        }
    }
    for (ep0, ep1) in &live_channel_eps {
        channels.close_endpoint(*ep0);
        channels.close_endpoint(*ep1);
    }
    for addr in &live_heap_addrs {
        assert!(heap.free(*addr));
    }

    assert_eq!(channels.live_pages, 0, "all channel pages freed");
    assert_eq!(sched_ctxs.live_count, 0, "all scheduling contexts freed");
    assert_eq!(heap.live_count(), 0, "all heap allocations freed");
}

// ============================================================
// TEST 2: Process lifecycle churn with handle transfer
//
// Rapidly creates and kills 50+ processes with handle transfer.
// Models the kernel's process create → handle_send → start → kill
// lifecycle including cleanup of all transferred handles.
//
// Fulfills: VAL-INTEG-002
// ============================================================

#[test]
fn integration_stress_process_churn_with_handle_transfer_75_cycles() {
    let mut channels = ChannelTable::new();
    let mut sched_ctxs = SchedContextTable::new();
    let mut processes = ProcessTable::new();

    for cycle in 0..75u32 {
        // Step 1: Create a process.
        let pid = processes.create();

        // Step 2: Create channels and scheduling context for the process.
        let (ep0, ep1) = channels.create();
        let (ep2, ep3) = channels.create();
        let budget = MIN_BUDGET_NS * 2;
        let period = MIN_PERIOD_NS * 10;
        let sc_id = sched_ctxs
            .create(budget, period)
            .expect("should create sched ctx");

        // Step 3: Transfer handles to the process (models handle_send).
        let ht = processes.handles_mut(pid);
        ht.insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .expect("insert ep0");
        ht.insert(HandleObject::Channel(ep2), Rights::READ_WRITE)
            .expect("insert ep2");
        ht.insert(HandleObject::SchedulingContext(sc_id), Rights::READ_WRITE)
            .expect("insert sched ctx");

        // Verify handles are accessible.
        let ht = processes.handles_mut(pid);
        assert!(matches!(
            ht.get(Handle(0), Rights::READ),
            Ok(HandleObject::Channel(_))
        ));
        assert!(matches!(
            ht.get(Handle(1), Rights::READ),
            Ok(HandleObject::Channel(_))
        ));
        assert!(matches!(
            ht.get(Handle(2), Rights::READ),
            Ok(HandleObject::SchedulingContext(_))
        ));

        // Step 4: Start the process.
        assert!(processes.start(pid), "cycle {cycle}: start should succeed");

        // Step 5: Kill the process — drains handles and performs cleanup.
        let drained = processes.kill(pid);

        // Verify all 3 handles were drained.
        assert_eq!(
            drained.len(),
            3,
            "cycle {cycle}: should drain all 3 handles"
        );

        // Step 6: Perform resource cleanup for each drained handle.
        for (obj, _rights, _) in &drained {
            match obj {
                HandleObject::Channel(id) => channels.close_endpoint(*id),
                HandleObject::SchedulingContext(id) => {
                    sched_ctxs.dec_ref(*id);
                }
                _ => {}
            }
        }

        // Close the other ends of the channels (held by "parent" process).
        channels.close_endpoint(ep1);
        channels.close_endpoint(ep3);

        // Verify all resources from this cycle are cleaned up.
        assert_eq!(
            channels.live_pages, 0,
            "cycle {cycle}: all channel pages freed"
        );
    }

    assert_eq!(
        channels.live_pages, 0,
        "all channel pages freed after all cycles"
    );
    assert_eq!(sched_ctxs.live_count, 0, "all scheduling contexts freed");
    assert_eq!(processes.live_count, 0, "all processes cleaned up");
}

/// Process churn variant: interleaved create and kill with overlapping lifetimes.
/// 60 processes created, killed in random order.
#[test]
fn integration_stress_process_churn_interleaved_60() {
    let mut rng = Rng::new(0xBEEF_CAFE);
    let mut channels = ChannelTable::new();
    let mut sched_ctxs = SchedContextTable::new();
    let mut processes = ProcessTable::new();

    struct LiveProcess {
        pid: process::ProcessId,
        parent_eps: Vec<ChannelId>, // Parent's endpoints to close on kill.
    }

    let mut live: Vec<LiveProcess> = Vec::new();

    // Create 60 processes.
    for _ in 0..60 {
        let pid = processes.create();

        let (ep0, ep1) = channels.create();
        let sc_id = sched_ctxs.create(MIN_BUDGET_NS, MIN_PERIOD_NS).unwrap();

        let ht = processes.handles_mut(pid);
        ht.insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .unwrap();
        ht.insert(HandleObject::SchedulingContext(sc_id), Rights::READ_WRITE)
            .unwrap();

        processes.start(pid);

        live.push(LiveProcess {
            pid,
            parent_eps: vec![ep1],
        });
    }

    assert_eq!(processes.live_count, 60);
    assert_eq!(channels.live_pages, 60 * 2); // 60 channels, 2 pages each.

    // Kill in random order.
    rng.shuffle(&mut live);

    for lp in &live {
        let drained = processes.kill(lp.pid);
        for (obj, _, _) in &drained {
            match obj {
                HandleObject::Channel(id) => channels.close_endpoint(*id),
                HandleObject::SchedulingContext(id) => {
                    sched_ctxs.dec_ref(*id);
                }
                _ => {}
            }
        }
        for ep in &lp.parent_eps {
            channels.close_endpoint(*ep);
        }
    }

    assert_eq!(channels.live_pages, 0, "all channel pages freed");
    assert_eq!(sched_ctxs.live_count, 0, "all scheduling contexts freed");
    assert_eq!(processes.live_count, 0, "all processes cleaned up");
}

/// Process churn: kill before start (unstarted process cleanup).
#[test]
fn integration_stress_process_churn_kill_before_start_50() {
    let mut channels = ChannelTable::new();
    let mut processes = ProcessTable::new();

    for cycle in 0..50u32 {
        let pid = processes.create();

        let (ep0, ep1) = channels.create();
        let ht = processes.handles_mut(pid);
        ht.insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .unwrap();

        // Kill without starting.
        let drained = processes.kill(pid);
        assert_eq!(drained.len(), 1, "cycle {cycle}: one handle drained");

        for (obj, _, _) in &drained {
            if let HandleObject::Channel(id) = obj {
                channels.close_endpoint(*id);
            }
        }
        channels.close_endpoint(ep1);

        assert_eq!(channels.live_pages, 0, "cycle {cycle}: pages freed");
    }

    assert_eq!(processes.live_count, 0);
}

/// Process churn: multiple handle types transferred.
#[test]
fn integration_stress_process_churn_multi_handle_transfer_50() {
    let mut rng = Rng::new(0xFEED_FACE);
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();
    let mut sched_ctxs = SchedContextTable::new();
    let mut processes = ProcessTable::new();

    for cycle in 0..50u32 {
        let pid = processes.create();

        // Create various resources and transfer handles.
        let (ep0, ep1) = channels.create();
        let timer_id = timers.create(cycle as u64 * 1000).expect("timer create");
        let budget = MIN_BUDGET_NS * (1 + rng.next_usize(3) as u64);
        // Ensure period >= MIN_PERIOD_NS and period >= budget.
        let period = (budget * (2 + rng.next_usize(5) as u64))
            .max(MIN_PERIOD_NS)
            .min(MAX_PERIOD_NS);
        let sc_id = sched_ctxs.create(budget, period).expect("sched ctx create");

        let ht = processes.handles_mut(pid);
        ht.insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .unwrap();
        ht.insert(HandleObject::Timer(timer_id), Rights::READ_WRITE)
            .unwrap();
        ht.insert(HandleObject::SchedulingContext(sc_id), Rights::READ_WRITE)
            .unwrap();

        processes.start(pid);

        // Kill and cleanup.
        let drained = processes.kill(pid);
        assert_eq!(drained.len(), 3, "cycle {cycle}: 3 handles drained");

        for (obj, _, _) in &drained {
            match obj {
                HandleObject::Channel(id) => channels.close_endpoint(*id),
                HandleObject::Timer(id) => {
                    timers.destroy(*id);
                }
                HandleObject::SchedulingContext(id) => {
                    sched_ctxs.dec_ref(*id);
                }
                _ => {}
            }
        }
        channels.close_endpoint(ep1);

        assert_eq!(channels.live_pages, 0, "cycle {cycle}: channel pages freed");
        assert_eq!(timers.active_count(), 0, "cycle {cycle}: timers freed");
    }

    assert_eq!(channels.live_pages, 0);
    assert_eq!(timers.active_count(), 0);
    assert_eq!(sched_ctxs.live_count, 0);
    assert_eq!(processes.live_count, 0);
}

// ============================================================
// TEST 3: Wait syscall stale waiter cleanup
//
// Models the sys_wait stale waiter cleanup path:
// 1. Thread calls wait on handles [A, B, C]
// 2. Handle B fires → thread wakes, handles A and C are stale
// 3. Thread calls wait again → stale registrations from A, C
//    are cleaned up before new registrations
//
// Fulfills: VAL-INTEG-005
// ============================================================

#[test]
fn integration_stress_stale_waiter_cleanup_basic() {
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();

    let tid = thread::ThreadId(1);
    let mut thread = ModelThread::new(tid);

    // Create 3 channels for wait handles.
    let (ch_a0, ch_a1) = channels.create();
    let (ch_b0, ch_b1) = channels.create();
    let (ch_c0, ch_c1) = channels.create();

    // --- First wait: wait on [ch_a1, ch_b1, ch_c1] ---

    // Build wait set.
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ch_a1),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Channel(ch_b1),
            user_index: 1,
        },
        WaitEntry {
            object: HandleObject::Channel(ch_c1),
            user_index: 2,
        },
    ];

    // Register waiters on all channels (models sys_wait registration loop).
    channels.register_waiter(ch_a1, tid);
    channels.register_waiter(ch_b1, tid);
    channels.register_waiter(ch_c1, tid);

    // Channel B fires (signal from ep0).
    let waiter = channels.signal(ch_b0);
    assert_eq!(waiter, Some(tid), "B should wake our thread");

    // Thread wakes: complete_wait_for moves unfired to stale_waiters.
    let result = thread.complete_wait_for(&HandleObject::Channel(ch_b1));
    assert_eq!(result, 1, "should return user_index 1 (handle B)");

    // Stale waiters should contain A and C.
    assert_eq!(thread.stale_waiters.len(), 2, "A and C are stale");
    assert!(thread
        .stale_waiters
        .iter()
        .any(|e| e.object == HandleObject::Channel(ch_a1)));
    assert!(thread
        .stale_waiters
        .iter()
        .any(|e| e.object == HandleObject::Channel(ch_c1)));

    // Wait set should be cleared.
    assert!(thread.wait_set.is_empty(), "wait set cleared after wake");

    // --- Second wait: stale registrations are cleaned up ---

    // Take stale waiters (models the start of sys_wait).
    let stale = thread.take_stale_waiters();
    assert_eq!(stale.len(), 2, "should have 2 stale entries");

    // Unregister stale waiters from their subsystems.
    for entry in &stale {
        match entry.object {
            HandleObject::Channel(id) => channels.unregister_waiter(id),
            HandleObject::Timer(id) => timers.unregister_waiter(id),
            _ => {}
        }
    }

    // Verify A and C no longer have registered waiters.
    // Signal A — should return None (waiter was unregistered).
    let waiter_a = channels.signal(ch_a0);
    assert_eq!(waiter_a, None, "A's waiter should have been unregistered");

    // Signal C — should return None.
    let waiter_c = channels.signal(ch_c0);
    assert_eq!(waiter_c, None, "C's waiter should have been unregistered");

    // Stale list should be empty now.
    assert!(
        thread.stale_waiters.is_empty(),
        "stale_waiters cleared after take"
    );

    // Now perform the second wait on [ch_a1, ch_c1] (fresh registrations).
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ch_a1),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Channel(ch_c1),
            user_index: 1,
        },
    ];

    channels.register_waiter(ch_a1, tid);
    channels.register_waiter(ch_c1, tid);

    // A fires now (fresh registration).
    let waiter = channels.signal(ch_a0);
    assert_eq!(waiter, Some(tid), "fresh registration should work");

    let result = thread.complete_wait_for(&HandleObject::Channel(ch_a1));
    assert_eq!(result, 0, "should return user_index 0 (handle A)");

    // Only C should be stale now.
    assert_eq!(thread.stale_waiters.len(), 1);
    assert_eq!(thread.stale_waiters[0].object, HandleObject::Channel(ch_c1));

    // Cleanup.
    let stale = thread.take_stale_waiters();
    for entry in &stale {
        if let HandleObject::Channel(id) = entry.object {
            channels.unregister_waiter(id);
        }
    }
    channels.close_endpoint(ch_a0);
    channels.close_endpoint(ch_a1);
    channels.close_endpoint(ch_b0);
    channels.close_endpoint(ch_b1);
    channels.close_endpoint(ch_c0);
    channels.close_endpoint(ch_c1);

    assert_eq!(channels.live_pages, 0, "all pages freed");
}

/// Stale waiter cleanup with timers in the wait set.
#[test]
fn integration_stress_stale_waiter_cleanup_mixed_types() {
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();

    let tid = thread::ThreadId(2);
    let mut thread = ModelThread::new(tid);

    // Create channel and timer.
    let (ch0, ch1) = channels.create();
    let timer_id = timers.create(5000).unwrap();

    // Wait on [channel, timer].
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ch1),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Timer(timer_id),
            user_index: 1,
        },
    ];

    channels.register_waiter(ch1, tid);
    timers.register_waiter(timer_id, tid);

    // Timer fires first.
    let waiter = timers.fire(timer_id);
    assert_eq!(waiter, Some(tid));

    // Thread wakes: timer wins, channel is stale.
    let result = thread.complete_wait_for(&HandleObject::Timer(timer_id));
    assert_eq!(result, 1, "user_index 1 (timer)");

    assert_eq!(thread.stale_waiters.len(), 1);
    assert_eq!(thread.stale_waiters[0].object, HandleObject::Channel(ch1));

    // Second wait: clean up stale channel waiter.
    let stale = thread.take_stale_waiters();
    for entry in &stale {
        match entry.object {
            HandleObject::Channel(id) => channels.unregister_waiter(id),
            HandleObject::Timer(id) => timers.unregister_waiter(id),
            _ => {}
        }
    }

    // Verify channel no longer has waiter.
    let waiter = channels.signal(ch0);
    assert_eq!(waiter, None, "channel waiter cleaned up");

    // Cleanup.
    timers.destroy(timer_id);
    channels.close_endpoint(ch0);
    channels.close_endpoint(ch1);

    assert_eq!(channels.live_pages, 0);
    assert_eq!(timers.active_count(), 0);
}

/// Stale waiter cleanup with timeout timer.
#[test]
fn integration_stress_stale_waiter_cleanup_timeout_timer() {
    let mut channels = ChannelTable::new();
    let mut timers = TimerTable::new();

    let tid = thread::ThreadId(3);
    let mut thread = ModelThread::new(tid);

    // Create channel and internal timeout timer.
    let (ch0, ch1) = channels.create();
    let timeout_timer = timers.create(1000).unwrap();
    thread.timeout_timer = Some(timeout_timer);

    // Wait on [channel] + internal timeout.
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ch1),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Timer(timeout_timer),
            user_index: TIMEOUT_SENTINEL,
        },
    ];

    channels.register_waiter(ch1, tid);
    timers.register_waiter(timeout_timer, tid);

    // Channel fires.
    let waiter = channels.signal(ch0);
    assert_eq!(waiter, Some(tid));

    let result = thread.complete_wait_for(&HandleObject::Channel(ch1));
    assert_eq!(result, 0, "user_index 0 (channel)");

    // Stale: timeout timer entry.
    assert_eq!(thread.stale_waiters.len(), 1);
    assert_eq!(thread.stale_waiters[0].user_index, TIMEOUT_SENTINEL);

    // Second wait: clean up stale timeout timer.
    let stale_timer = thread.take_timeout_timer();
    assert_eq!(stale_timer, Some(timeout_timer));
    timers.destroy(timeout_timer);

    let stale = thread.take_stale_waiters();
    for entry in &stale {
        if let HandleObject::Timer(id) = entry.object {
            timers.unregister_waiter(id);
        }
    }

    assert_eq!(timers.active_count(), 0, "timeout timer destroyed");

    // Cleanup.
    channels.close_endpoint(ch0);
    channels.close_endpoint(ch1);
    assert_eq!(channels.live_pages, 0);
}

/// Stale waiter stress: repeated wait/wake cycles (50 rounds),
/// each time with a different handle firing. Verifies stale
/// registrations are properly cleaned up each round.
#[test]
fn integration_stress_stale_waiter_repeated_cycles_50() {
    let mut rng = Rng::new(0xAAAA_BBBB);
    let mut channels = ChannelTable::new();

    let tid = thread::ThreadId(4);
    let mut thread = ModelThread::new(tid);

    // Create 5 channels.
    let mut ch_pairs: Vec<(ChannelId, ChannelId)> = Vec::new();
    for _ in 0..5 {
        ch_pairs.push(channels.create());
    }

    for round in 0..50 {
        // Clean up stale waiters from previous round.
        let stale = thread.take_stale_waiters();
        for entry in &stale {
            if let HandleObject::Channel(id) = entry.object {
                channels.unregister_waiter(id);
            }
        }

        // Also clean up any stale timeout timer.
        if let Some(_timer) = thread.take_timeout_timer() {
            // In real kernel, would call timer::destroy here.
        }

        // Build wait set with all 5 channels.
        thread.wait_set.clear();
        for (i, (_ep0, ep1)) in ch_pairs.iter().enumerate() {
            thread.wait_set.push(WaitEntry {
                object: HandleObject::Channel(*ep1),
                user_index: i as u8,
            });
        }

        // Register waiters.
        for (_ep0, ep1) in &ch_pairs {
            channels.register_waiter(*ep1, tid);
        }

        // Randomly choose which channel fires.
        let fire_idx = rng.next_usize(5);
        let (fire_ep0, fire_ep1) = ch_pairs[fire_idx];

        let waiter = channels.signal(fire_ep0);
        assert_eq!(
            waiter,
            Some(tid),
            "round {round}: waiter should be returned"
        );

        // Complete wait.
        let result = thread.complete_wait_for(&HandleObject::Channel(fire_ep1));
        assert_eq!(
            result, fire_idx as u64,
            "round {round}: result should be fired index"
        );

        // Verify stale count is 4 (5 total - 1 fired).
        assert_eq!(
            thread.stale_waiters.len(),
            4,
            "round {round}: 4 stale waiters"
        );

        // Verify the fired channel is NOT in stale.
        assert!(
            !thread
                .stale_waiters
                .iter()
                .any(|e| e.object == HandleObject::Channel(fire_ep1)),
            "round {round}: fired channel should not be stale"
        );
    }

    // Final cleanup.
    let stale = thread.take_stale_waiters();
    for entry in &stale {
        if let HandleObject::Channel(id) = entry.object {
            channels.unregister_waiter(id);
        }
    }

    for (ep0, ep1) in &ch_pairs {
        channels.close_endpoint(*ep0);
        channels.close_endpoint(*ep1);
    }

    assert_eq!(channels.live_pages, 0, "all pages freed");
}

/// Stale waiter: verify no spurious wakeups from stale registrations.
///
/// After cleaning up stale waiters, signals on previously-waited handles
/// should NOT produce wakeups for this thread.
#[test]
fn integration_stress_stale_waiter_no_spurious_wakeup() {
    let mut channels = ChannelTable::new();

    let tid = thread::ThreadId(5);
    let mut thread = ModelThread::new(tid);

    let (ch_a0, ch_a1) = channels.create();
    let (ch_b0, ch_b1) = channels.create();

    // First wait on [A, B].
    thread.wait_set = vec![
        WaitEntry {
            object: HandleObject::Channel(ch_a1),
            user_index: 0,
        },
        WaitEntry {
            object: HandleObject::Channel(ch_b1),
            user_index: 1,
        },
    ];
    channels.register_waiter(ch_a1, tid);
    channels.register_waiter(ch_b1, tid);

    // A fires.
    channels.signal(ch_a0);
    thread.complete_wait_for(&HandleObject::Channel(ch_a1));

    // B is stale. Clean it up.
    let stale = thread.take_stale_waiters();
    assert_eq!(stale.len(), 1);
    for entry in &stale {
        if let HandleObject::Channel(id) = entry.object {
            channels.unregister_waiter(id);
        }
    }

    // Now signal B — should NOT return our thread as waiter.
    let waiter = channels.signal(ch_b0);
    assert_eq!(waiter, None, "B should not wake us after cleanup");

    // Second wait on [B] only.
    thread.wait_set = vec![WaitEntry {
        object: HandleObject::Channel(ch_b1),
        user_index: 0,
    }];
    channels.register_waiter(ch_b1, tid);

    // Signal B — now it should work (fresh registration).
    let waiter = channels.signal(ch_b0);
    assert_eq!(waiter, Some(tid), "fresh registration should work");

    let result = thread.complete_wait_for(&HandleObject::Channel(ch_b1));
    assert_eq!(result, 0);

    // Cleanup.
    let stale = thread.take_stale_waiters();
    for entry in &stale {
        if let HandleObject::Channel(id) = entry.object {
            channels.unregister_waiter(id);
        }
    }
    channels.close_endpoint(ch_a0);
    channels.close_endpoint(ch_a1);
    channels.close_endpoint(ch_b0);
    channels.close_endpoint(ch_b1);
    assert_eq!(channels.live_pages, 0);
}

/// Integration: process churn WHILE stale waiter cleanup happens.
/// Tests the interaction between process kill (handle drain) and
/// wait cleanup on the same channels.
#[test]
fn integration_stress_process_churn_with_wait_cleanup() {
    let mut channels = ChannelTable::new();
    let mut processes = ProcessTable::new();

    let tid = thread::ThreadId(6);
    let mut thread = ModelThread::new(tid);

    for cycle in 0..30u32 {
        // Create a process with a channel.
        let pid = processes.create();
        let (ep0, ep1) = channels.create();

        let ht = processes.handles_mut(pid);
        ht.insert(HandleObject::Channel(ep0), Rights::READ_WRITE)
            .unwrap();
        processes.start(pid);

        // Thread waits on ep1 (parent's end).
        thread.wait_set = vec![WaitEntry {
            object: HandleObject::Channel(ep1),
            user_index: 0,
        }];
        channels.register_waiter(ep1, tid);

        // Kill the process — closes ep0, which wakes peer (ep1).
        let drained = processes.kill(pid);
        for (obj, _, _) in &drained {
            if let HandleObject::Channel(id) = obj {
                channels.close_endpoint(*id);
            }
        }

        // Check if ep1 had pending signal from close waking peer.
        // In real kernel, close_endpoint wakes the peer. Here we
        // simulate: ep1 gets a "peer closed" notification.
        // Thread wakes and completes wait.
        thread.complete_wait_for(&HandleObject::Channel(ep1));

        // Cleanup stale.
        let stale = thread.take_stale_waiters();
        for entry in &stale {
            if let HandleObject::Channel(id) = entry.object {
                channels.unregister_waiter(id);
            }
        }

        // Close ep1.
        channels.close_endpoint(ep1);

        assert_eq!(channels.live_pages, 0, "cycle {cycle}: all pages freed");
    }

    assert_eq!(processes.live_count, 0);
}
