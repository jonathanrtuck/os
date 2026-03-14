//! Host-side tests for handle lifecycle verification.
//!
//! Traces creation → transfer → close → cleanup for every handle type:
//! Channel, Timer, Interrupt, Thread, Process, SchedulingContext.
//!
//! Models the kernel's handle lifecycle across handle.rs, syscall.rs,
//! process.rs, channel.rs, timer.rs, interrupt.rs, thread_exit.rs,
//! process_exit.rs, and scheduling_context.rs. Verifies no leak paths.

// ============================================================
// Minimal kernel models (host-side stubs)
// ============================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThreadId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimerId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterruptId(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SchedulingContextId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HandleObject {
    Channel(ChannelId),
    Timer(TimerId),
    Interrupt(InterruptId),
    Thread(ThreadId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rights(u32);

impl Rights {
    const READ: Self = Self(1);
    const READ_WRITE: Self = Self(3);
}

// --- Handle table model ---

const MAX_HANDLES: usize = 256;

struct HandleEntry {
    object: HandleObject,
    rights: Rights,
}

struct HandleTable {
    entries: [Option<HandleEntry>; MAX_HANDLES],
}

impl HandleTable {
    fn new() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
        }
    }

    fn insert(&mut self, obj: HandleObject, rights: Rights) -> Option<u8> {
        for (i, slot) in self.entries.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(HandleEntry {
                    object: obj,
                    rights,
                });
                return Some(i as u8);
            }
        }
        None
    }

    fn close(&mut self, handle: u8) -> Option<(HandleObject, Rights)> {
        let slot = &mut self.entries[handle as usize];
        let entry = slot.take()?;
        Some((entry.object, entry.rights))
    }

    fn insert_at(&mut self, handle: u8, obj: HandleObject, rights: Rights) -> bool {
        let slot = &mut self.entries[handle as usize];
        if slot.is_some() {
            return false;
        }
        *slot = Some(HandleEntry {
            object: obj,
            rights,
        });
        true
    }

    fn drain(&mut self) -> Vec<(HandleObject, Rights)> {
        let mut result = Vec::new();
        for slot in self.entries.iter_mut() {
            if let Some(entry) = slot.take() {
                result.push((entry.object, entry.rights));
            }
        }
        result
    }

    fn count(&self) -> usize {
        self.entries.iter().filter(|s| s.is_some()).count()
    }
}

// --- Channel model ---

struct Channel {
    pages: [u64; 2], // physical page addresses
    pending_signal: [bool; 2],
    waiter: [Option<ThreadId>; 2],
    closed_count: u8,
}

impl Channel {
    fn new(page0: u64, page1: u64) -> Self {
        Self {
            pages: [page0, page1],
            pending_signal: [false, false],
            waiter: [None, None],
            closed_count: 0,
        }
    }

    fn endpoint_index(id: ChannelId) -> usize {
        id.0 as usize % 2
    }

    fn close_endpoint(&mut self, id: ChannelId) -> (bool, Option<ThreadId>) {
        if self.closed_count >= 2 {
            return (false, None);
        }
        let ep = Self::endpoint_index(id);
        let peer_ep = 1 - ep;
        self.waiter[ep] = None;
        let peer_waiter = self.waiter[peer_ep].take();
        self.closed_count += 1;
        let pages_freed = self.closed_count == 2;
        if pages_freed {
            self.pages = [0, 0];
        }
        (pages_freed, peer_waiter)
    }

    fn is_fully_closed(&self) -> bool {
        self.closed_count >= 2
    }
}

// --- Timer model ---

struct Timer {
    deadline_ticks: u64,
    fired: bool,
}

struct TimerTable {
    slots: [Option<Timer>; 32],
    waiters: WaitableRegistry,
}

impl TimerTable {
    fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            waiters: WaitableRegistry::new(),
        }
    }

    fn create(&mut self, timeout_ticks: u64) -> Option<TimerId> {
        for i in 0..32 {
            if self.slots[i].is_none() {
                self.slots[i] = Some(Timer {
                    deadline_ticks: timeout_ticks,
                    fired: false,
                });
                self.waiters.create(i);
                return Some(TimerId(i as u8));
            }
        }
        None
    }

    fn destroy(&mut self, id: TimerId) -> Option<ThreadId> {
        self.slots[id.0 as usize] = None;
        self.waiters.destroy(id.0 as usize)
    }

    fn is_allocated(&self, id: TimerId) -> bool {
        self.slots[id.0 as usize].is_some()
    }
}

// --- Interrupt model ---

struct InterruptTable {
    slots: [Option<u32>; 32], // IRQ number
    waiters: WaitableRegistry,
}

impl InterruptTable {
    fn new() -> Self {
        Self {
            slots: [None; 32],
            waiters: WaitableRegistry::new(),
        }
    }

    fn register(&mut self, irq: u32) -> Option<InterruptId> {
        // Reject duplicates
        for slot in &self.slots {
            if *slot == Some(irq) {
                return None;
            }
        }
        for i in 0..32 {
            if self.slots[i].is_none() {
                self.slots[i] = Some(irq);
                self.waiters.create(i);
                return Some(InterruptId(i as u8));
            }
        }
        None
    }

    fn destroy(&mut self, id: InterruptId) -> Option<ThreadId> {
        self.slots[id.0 as usize] = None;
        self.waiters.destroy(id.0 as usize)
    }

    fn is_registered(&self, id: InterruptId) -> bool {
        self.slots[id.0 as usize].is_some()
    }
}

// --- WaitableRegistry model ---

struct WaitableEntry {
    ready: bool,
    waiter: Option<ThreadId>,
}

struct WaitableRegistry {
    entries: Vec<Option<WaitableEntry>>,
}

impl WaitableRegistry {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn create(&mut self, idx: usize) {
        if idx >= self.entries.len() {
            self.entries.resize_with(idx + 1, || None);
        }
        self.entries[idx] = Some(WaitableEntry {
            ready: false,
            waiter: None,
        });
    }

    fn destroy(&mut self, idx: usize) -> Option<ThreadId> {
        if let Some(slot) = self.entries.get_mut(idx) {
            let waiter = slot.as_mut().and_then(|e| e.waiter.take());
            *slot = None;
            waiter
        } else {
            None
        }
    }

    fn notify(&mut self, idx: usize) -> Option<ThreadId> {
        let entry = self.entries.get_mut(idx)?.as_mut()?;
        entry.ready = true;
        entry.waiter.take()
    }

    fn check_ready(&self, idx: usize) -> bool {
        self.entries
            .get(idx)
            .and_then(|s| s.as_ref())
            .is_some_and(|e| e.ready)
    }

    fn exists(&self, idx: usize) -> bool {
        self.entries.get(idx).and_then(|s| s.as_ref()).is_some()
    }
}

// --- SchedulingContext model ---

struct SchedulingContextSlot {
    budget: u64,
    period: u64,
    ref_count: u32,
}

struct SchedulingContextTable {
    slots: Vec<Option<SchedulingContextSlot>>,
    free_ids: Vec<u32>,
}

impl SchedulingContextTable {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_ids: Vec::new(),
        }
    }

    fn create(&mut self, budget: u64, period: u64) -> SchedulingContextId {
        let slot = SchedulingContextSlot {
            budget,
            period,
            ref_count: 1,
        };
        let id = if let Some(free_id) = self.free_ids.pop() {
            self.slots[free_id as usize] = Some(slot);
            SchedulingContextId(free_id)
        } else {
            let len = self.slots.len();
            self.slots.push(Some(slot));
            SchedulingContextId(len as u32)
        };
        id
    }

    fn release(&mut self, id: SchedulingContextId) {
        if let Some(slot) = self.slots.get_mut(id.0 as usize) {
            if let Some(entry) = slot {
                entry.ref_count = entry.ref_count.saturating_sub(1);
                if entry.ref_count == 0 {
                    *slot = None;
                    self.free_ids.push(id.0);
                }
            }
        }
    }

    fn increment_ref(&mut self, id: SchedulingContextId) {
        if let Some(Some(entry)) = self.slots.get_mut(id.0 as usize) {
            entry.ref_count += 1;
        }
    }

    fn ref_count(&self, id: SchedulingContextId) -> u32 {
        self.slots
            .get(id.0 as usize)
            .and_then(|s| s.as_ref())
            .map(|s| s.ref_count)
            .unwrap_or(0)
    }

    fn is_allocated(&self, id: SchedulingContextId) -> bool {
        self.slots
            .get(id.0 as usize)
            .and_then(|s| s.as_ref())
            .is_some()
    }
}

// --- Handle categorization model (mirrors scheduler::categorize_handles) ---

struct HandleCategories {
    channels: Vec<ChannelId>,
    interrupts: Vec<InterruptId>,
    timers: Vec<TimerId>,
    thread_handles: Vec<ThreadId>,
    process_handles: Vec<ProcessId>,
    scheduling_contexts: Vec<SchedulingContextId>,
}

fn categorize_handles(objects: Vec<(HandleObject, Rights)>) -> HandleCategories {
    let mut cats = HandleCategories {
        channels: Vec::new(),
        interrupts: Vec::new(),
        timers: Vec::new(),
        thread_handles: Vec::new(),
        process_handles: Vec::new(),
        scheduling_contexts: Vec::new(),
    };
    for (obj, _) in objects {
        match obj {
            HandleObject::Channel(id) => cats.channels.push(id),
            HandleObject::Interrupt(id) => cats.interrupts.push(id),
            HandleObject::Timer(id) => cats.timers.push(id),
            HandleObject::Thread(id) => cats.thread_handles.push(id),
            HandleObject::Process(id) => cats.process_handles.push(id),
            HandleObject::SchedulingContext(id) => cats.scheduling_contexts.push(id),
        }
    }
    cats
}

// ============================================================
// Channel handle lifecycle
// ============================================================

#[test]
fn channel_lifecycle_create_close_cleanup() {
    // Trace: channel::create → handle insert → handle_close → close_endpoint → page free
    let mut ch = Channel::new(0x1000, 0x2000);
    let mut table = HandleTable::new();
    let ep_a = ChannelId(0);
    let ep_b = ChannelId(1);

    // Create: insert both handles
    let h_a = table
        .insert(HandleObject::Channel(ep_a), Rights::READ_WRITE)
        .unwrap();
    let h_b = table
        .insert(HandleObject::Channel(ep_b), Rights::READ_WRITE)
        .unwrap();
    assert_eq!(table.count(), 2);

    // Close ep_a handle
    let (obj_a, _) = table.close(h_a).unwrap();
    assert!(matches!(obj_a, HandleObject::Channel(ChannelId(0))));
    let (freed_a, _) = ch.close_endpoint(ep_a);
    assert!(!freed_a, "one endpoint closed — pages not freed yet");
    assert!(!ch.is_fully_closed());

    // Close ep_b handle
    let (obj_b, _) = table.close(h_b).unwrap();
    assert!(matches!(obj_b, HandleObject::Channel(ChannelId(1))));
    let (freed_b, _) = ch.close_endpoint(ep_b);
    assert!(freed_b, "both endpoints closed — pages freed");
    assert!(ch.is_fully_closed());

    // No handles remain
    assert_eq!(table.count(), 0);
}

#[test]
fn channel_lifecycle_transfer_then_close() {
    // Trace: create → transfer via handle_send (move) → close in target → cleanup
    let mut ch = Channel::new(0x1000, 0x2000);
    let mut source_table = HandleTable::new();
    let mut target_table = HandleTable::new();
    let ep_a = ChannelId(0);
    let ep_b = ChannelId(1);

    // Source creates channel, gets both endpoints
    let h_a = source_table
        .insert(HandleObject::Channel(ep_a), Rights::READ_WRITE)
        .unwrap();
    let h_b = source_table
        .insert(HandleObject::Channel(ep_b), Rights::READ_WRITE)
        .unwrap();

    // Transfer ep_b to target (move semantics: close from source, insert into target)
    let (obj_b, rights_b) = source_table.close(h_b).unwrap();
    let t_b = target_table.insert(obj_b, rights_b).unwrap();

    assert_eq!(source_table.count(), 1, "source keeps ep_a only");
    assert_eq!(target_table.count(), 1, "target has ep_b");

    // Target closes ep_b
    let (obj, _) = target_table.close(t_b).unwrap();
    assert!(matches!(obj, HandleObject::Channel(ChannelId(1))));
    let (freed, _) = ch.close_endpoint(ep_b);
    assert!(!freed);

    // Source closes ep_a
    let (obj, _) = source_table.close(h_a).unwrap();
    assert!(matches!(obj, HandleObject::Channel(ChannelId(0))));
    let (freed, _) = ch.close_endpoint(ep_a);
    assert!(freed, "channel fully cleaned up");

    assert_eq!(source_table.count(), 0);
    assert_eq!(target_table.count(), 0);
}

#[test]
fn channel_lifecycle_process_exit_drain() {
    // Trace: process exit → drain handle table → close_endpoint for each channel
    let mut ch = Channel::new(0x1000, 0x2000);
    let mut table = HandleTable::new();
    let ep_a = ChannelId(0);

    table
        .insert(HandleObject::Channel(ep_a), Rights::READ_WRITE)
        .unwrap();

    // Process exits — drain all handles
    let drained = table.drain();
    assert_eq!(drained.len(), 1);

    // Cleanup each handle object
    let cats = categorize_handles(drained);
    assert_eq!(cats.channels.len(), 1);
    assert_eq!(cats.channels[0], ep_a);

    // close_endpoint for each
    for id in &cats.channels {
        ch.close_endpoint(*id);
    }

    assert_eq!(ch.closed_count, 1, "one endpoint closed by process exit");
    assert_eq!(table.count(), 0, "table fully drained");
}

#[test]
fn channel_double_close_prevented() {
    // Verify close_endpoint returns early if already fully closed
    let mut ch = Channel::new(0x1000, 0x2000);
    ch.close_endpoint(ChannelId(0));
    ch.close_endpoint(ChannelId(1));

    // Third close attempt — guard prevents double-free
    let (freed, _) = ch.close_endpoint(ChannelId(0));
    assert!(!freed, "double close prevented — no pages freed again");
}

// ============================================================
// Timer handle lifecycle
// ============================================================

#[test]
fn timer_lifecycle_create_close_cleanup() {
    // Trace: timer::create → handle insert → handle_close → timer::destroy
    let mut timers = TimerTable::new();
    let mut table = HandleTable::new();

    let timer_id = timers.create(1000).unwrap();
    let h = table
        .insert(HandleObject::Timer(timer_id), Rights::READ)
        .unwrap();

    assert!(timers.is_allocated(timer_id));
    assert_eq!(table.count(), 1);

    // Close handle → destroy timer
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Timer(TimerId(0))));
    timers.destroy(timer_id);

    assert!(!timers.is_allocated(timer_id), "timer slot freed");
    assert_eq!(table.count(), 0);
}

#[test]
fn timer_lifecycle_process_exit_drain() {
    // Process exits with active timers — all destroyed
    let mut timers = TimerTable::new();
    let mut table = HandleTable::new();

    let t1 = timers.create(100).unwrap();
    let t2 = timers.create(200).unwrap();
    table.insert(HandleObject::Timer(t1), Rights::READ).unwrap();
    table.insert(HandleObject::Timer(t2), Rights::READ).unwrap();

    // Process exit drain
    let drained = table.drain();
    let cats = categorize_handles(drained);
    assert_eq!(cats.timers.len(), 2);

    for id in &cats.timers {
        timers.destroy(*id);
    }

    assert!(!timers.is_allocated(t1));
    assert!(!timers.is_allocated(t2));
}

#[test]
fn timer_create_rollback_on_handle_table_full() {
    // If handle table is full after timer::create, timer must be destroyed
    let mut timers = TimerTable::new();
    let mut table = HandleTable::new();

    // Fill table
    for i in 0..256u32 {
        table
            .insert(HandleObject::Channel(ChannelId(i)), Rights::READ)
            .unwrap();
    }

    let timer_id = timers.create(500).unwrap();
    assert!(timers.is_allocated(timer_id));

    // Handle insert fails
    let result = table.insert(HandleObject::Timer(timer_id), Rights::READ);
    assert!(result.is_none(), "table full");

    // Rollback: destroy the timer
    timers.destroy(timer_id);
    assert!(
        !timers.is_allocated(timer_id),
        "timer cleaned up on rollback"
    );
}

// ============================================================
// Interrupt handle lifecycle
// ============================================================

#[test]
fn interrupt_lifecycle_create_close_cleanup() {
    // Trace: interrupt::register → handle insert → handle_close → interrupt::destroy
    let mut ints = InterruptTable::new();
    let mut table = HandleTable::new();

    let int_id = ints.register(47).unwrap();
    let h = table
        .insert(HandleObject::Interrupt(int_id), Rights::READ_WRITE)
        .unwrap();

    assert!(ints.is_registered(int_id));

    // Close handle → destroy interrupt
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Interrupt(InterruptId(0))));
    ints.destroy(int_id);

    assert!(!ints.is_registered(int_id), "IRQ unregistered");
}

#[test]
fn interrupt_lifecycle_process_exit_drain() {
    // Process exits with registered interrupts — all destroyed
    let mut ints = InterruptTable::new();
    let mut table = HandleTable::new();

    let i1 = ints.register(30).unwrap();
    let i2 = ints.register(31).unwrap();
    table
        .insert(HandleObject::Interrupt(i1), Rights::READ_WRITE)
        .unwrap();
    table
        .insert(HandleObject::Interrupt(i2), Rights::READ_WRITE)
        .unwrap();

    let drained = table.drain();
    let cats = categorize_handles(drained);
    assert_eq!(cats.interrupts.len(), 2);

    for id in &cats.interrupts {
        ints.destroy(*id);
    }

    assert!(!ints.is_registered(i1));
    assert!(!ints.is_registered(i2));
}

#[test]
fn interrupt_create_rollback_on_handle_table_full() {
    let mut ints = InterruptTable::new();
    let mut table = HandleTable::new();

    for i in 0..256u32 {
        table
            .insert(HandleObject::Channel(ChannelId(i)), Rights::READ)
            .unwrap();
    }

    let int_id = ints.register(42).unwrap();
    assert!(ints.is_registered(int_id));

    let result = table.insert(HandleObject::Interrupt(int_id), Rights::READ_WRITE);
    assert!(result.is_none());

    // Rollback
    ints.destroy(int_id);
    assert!(!ints.is_registered(int_id));
}

// ============================================================
// Thread handle lifecycle
// ============================================================

#[test]
fn thread_lifecycle_create_close_cleanup() {
    // Trace: spawn_user → thread_exit::create → handle insert → handle_close → thread_exit::destroy
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let tid = ThreadId(5);

    // Thread created
    exit_reg.create(tid.0 as usize);
    let h = table
        .insert(HandleObject::Thread(tid), Rights::READ)
        .unwrap();

    assert!(exit_reg.exists(tid.0 as usize));

    // Close handle → destroy exit notification
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Thread(ThreadId(5))));
    exit_reg.destroy(tid.0 as usize);

    assert!(!exit_reg.exists(tid.0 as usize));
}

#[test]
fn thread_lifecycle_exit_then_handle_close() {
    // Thread exits first, parent closes handle later
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let tid = ThreadId(10);

    exit_reg.create(tid.0 as usize);
    let h = table
        .insert(HandleObject::Thread(tid), Rights::READ)
        .unwrap();

    // Thread exits — notify
    let waiter = exit_reg.notify(tid.0 as usize);
    assert!(waiter.is_none(), "no waiter registered");
    assert!(exit_reg.check_ready(tid.0 as usize));

    // Parent closes handle later
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Thread(ThreadId(10))));
    exit_reg.destroy(tid.0 as usize);

    assert!(!exit_reg.exists(tid.0 as usize));
}

#[test]
fn thread_lifecycle_process_exit_drain() {
    // Process exits with thread handles
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let tid1 = ThreadId(1);
    let tid2 = ThreadId(2);

    exit_reg.create(tid1.0 as usize);
    exit_reg.create(tid2.0 as usize);
    table
        .insert(HandleObject::Thread(tid1), Rights::READ)
        .unwrap();
    table
        .insert(HandleObject::Thread(tid2), Rights::READ)
        .unwrap();

    let drained = table.drain();
    let cats = categorize_handles(drained);
    assert_eq!(cats.thread_handles.len(), 2);

    for id in &cats.thread_handles {
        exit_reg.destroy(id.0 as usize);
    }

    assert!(!exit_reg.exists(tid1.0 as usize));
    assert!(!exit_reg.exists(tid2.0 as usize));
}

#[test]
fn thread_handle_create_rollback_on_table_full() {
    // thread_exit::create then handle fails → thread_exit::destroy
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let tid = ThreadId(99);

    for i in 0..256u32 {
        table
            .insert(HandleObject::Channel(ChannelId(i)), Rights::READ)
            .unwrap();
    }

    exit_reg.create(tid.0 as usize);
    assert!(exit_reg.exists(tid.0 as usize));

    let result = table.insert(HandleObject::Thread(tid), Rights::READ);
    assert!(result.is_none());

    // Rollback: destroy exit notification
    exit_reg.destroy(tid.0 as usize);
    assert!(!exit_reg.exists(tid.0 as usize));
}

// ============================================================
// Process handle lifecycle
// ============================================================

#[test]
fn process_lifecycle_create_close_cleanup() {
    // Trace: process_create → process_exit::create → handle insert → handle_close → process_exit::destroy
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let pid = ProcessId(3);

    exit_reg.create(pid.0 as usize);
    let h = table
        .insert(HandleObject::Process(pid), Rights::READ_WRITE)
        .unwrap();

    assert!(exit_reg.exists(pid.0 as usize));

    // Close handle → destroy exit notification
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Process(ProcessId(3))));
    exit_reg.destroy(pid.0 as usize);

    assert!(!exit_reg.exists(pid.0 as usize));
}

#[test]
fn process_lifecycle_exit_then_handle_close() {
    // Process exits, parent closes handle later
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let pid = ProcessId(7);

    exit_reg.create(pid.0 as usize);
    let h = table
        .insert(HandleObject::Process(pid), Rights::READ_WRITE)
        .unwrap();

    // Process's last thread exits
    let waiter = exit_reg.notify(pid.0 as usize);
    assert!(waiter.is_none());
    assert!(exit_reg.check_ready(pid.0 as usize));

    // Parent closes process handle
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::Process(ProcessId(7))));
    exit_reg.destroy(pid.0 as usize);

    assert!(!exit_reg.exists(pid.0 as usize));
}

#[test]
fn process_lifecycle_process_exit_drain() {
    // Process exits holding process handles of children
    let mut exit_reg = WaitableRegistry::new();
    let mut table = HandleTable::new();
    let child_pid = ProcessId(5);

    exit_reg.create(child_pid.0 as usize);
    table
        .insert(HandleObject::Process(child_pid), Rights::READ_WRITE)
        .unwrap();

    let drained = table.drain();
    let cats = categorize_handles(drained);
    assert_eq!(cats.process_handles.len(), 1);

    for id in &cats.process_handles {
        exit_reg.destroy(id.0 as usize);
    }

    assert!(!exit_reg.exists(child_pid.0 as usize));
}

// ============================================================
// SchedulingContext handle lifecycle
// ============================================================

#[test]
fn sched_ctx_lifecycle_create_close_cleanup() {
    // Trace: create (ref_count=1) → handle insert → handle_close → release (ref_count→0, freed)
    let mut ctx_table = SchedulingContextTable::new();
    let mut table = HandleTable::new();

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);

    let h = table
        .insert(HandleObject::SchedulingContext(ctx_id), Rights::READ_WRITE)
        .unwrap();

    // Close handle → release
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(
        obj,
        HandleObject::SchedulingContext(SchedulingContextId(0))
    ));
    ctx_table.release(ctx_id);

    assert_eq!(ctx_table.ref_count(ctx_id), 0);
    assert!(!ctx_table.is_allocated(ctx_id), "freed at ref_count=0");
}

#[test]
fn sched_ctx_lifecycle_bind_then_close() {
    // Trace: create (ref=1) → bind (ref=2) → handle_close (ref=1) → thread exit (ref=0, freed)
    let mut ctx_table = SchedulingContextTable::new();
    let mut table = HandleTable::new();

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    let h = table
        .insert(HandleObject::SchedulingContext(ctx_id), Rights::READ_WRITE)
        .unwrap();

    // Bind increments ref_count
    ctx_table.increment_ref(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 2);

    // Close handle → release (ref=1)
    let (obj, _) = table.close(h).unwrap();
    assert!(matches!(obj, HandleObject::SchedulingContext(_)));
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);
    assert!(
        ctx_table.is_allocated(ctx_id),
        "still alive — bound thread holds ref"
    );

    // Thread exits → release (ref=0)
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 0);
    assert!(!ctx_table.is_allocated(ctx_id), "freed after thread exit");
}

#[test]
fn sched_ctx_lifecycle_borrow_and_return() {
    // Trace: create (ref=1) → bind (ref=2) → borrow (ref=3) → return (ref=2) → unbind+close (ref=0)
    let mut ctx_table = SchedulingContextTable::new();

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);

    // Bind
    ctx_table.increment_ref(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 2);

    // Borrow
    ctx_table.increment_ref(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 3);

    // Return (releases borrow ref)
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 2);

    // Thread exit (releases bind ref)
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);

    // Handle close (releases handle ref)
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 0);
    assert!(!ctx_table.is_allocated(ctx_id));
}

#[test]
fn sched_ctx_lifecycle_create_rollback_on_handle_table_full() {
    // create (ref=1) → handle insert fails → release (ref=0, freed)
    let mut ctx_table = SchedulingContextTable::new();
    let mut table = HandleTable::new();

    for i in 0..256u32 {
        table
            .insert(HandleObject::Channel(ChannelId(i)), Rights::READ)
            .unwrap();
    }

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    assert!(ctx_table.is_allocated(ctx_id));

    let result = table.insert(HandleObject::SchedulingContext(ctx_id), Rights::READ_WRITE);
    assert!(result.is_none());

    // Rollback: release
    ctx_table.release(ctx_id);
    assert!(!ctx_table.is_allocated(ctx_id));
}

#[test]
fn sched_ctx_lifecycle_process_exit_drain() {
    // Process exits holding scheduling context handles → all released
    let mut ctx_table = SchedulingContextTable::new();
    let mut table = HandleTable::new();

    let ctx1 = ctx_table.create(1_000_000, 10_000_000);
    let ctx2 = ctx_table.create(2_000_000, 20_000_000);
    table
        .insert(HandleObject::SchedulingContext(ctx1), Rights::READ_WRITE)
        .unwrap();
    table
        .insert(HandleObject::SchedulingContext(ctx2), Rights::READ_WRITE)
        .unwrap();

    let drained = table.drain();
    let cats = categorize_handles(drained);
    assert_eq!(cats.scheduling_contexts.len(), 2);

    // Release each (mirrors categorize_handles in kernel which releases immediately)
    for id in &cats.scheduling_contexts {
        ctx_table.release(*id);
    }

    assert!(!ctx_table.is_allocated(ctx1));
    assert!(!ctx_table.is_allocated(ctx2));
}

#[test]
fn sched_ctx_lifecycle_transfer_preserves_refcount() {
    // handle_send moves (not copies) the handle — ref_count stays the same
    let mut ctx_table = SchedulingContextTable::new();
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);

    let h = source
        .insert(HandleObject::SchedulingContext(ctx_id), Rights::READ_WRITE)
        .unwrap();

    // Move: close from source
    let (obj, rights) = source.close(h).unwrap();
    assert_eq!(
        ctx_table.ref_count(ctx_id),
        1,
        "ref_count unchanged by move"
    );

    // Insert into target
    target.insert(obj, rights).unwrap();
    assert_eq!(
        ctx_table.ref_count(ctx_id),
        1,
        "ref_count still 1 — same logical reference"
    );

    // Target closes
    let (obj2, _) = target.close(0).unwrap();
    assert!(matches!(obj2, HandleObject::SchedulingContext(_)));
    ctx_table.release(ctx_id);
    assert_eq!(ctx_table.ref_count(ctx_id), 0);
    assert!(!ctx_table.is_allocated(ctx_id));
}

#[test]
fn sched_ctx_id_reuse_after_free() {
    // Freed IDs are pushed to free_ids and reused
    let mut ctx_table = SchedulingContextTable::new();

    let id0 = ctx_table.create(1_000_000, 10_000_000);
    let id1 = ctx_table.create(2_000_000, 20_000_000);
    assert_eq!(id0, SchedulingContextId(0));
    assert_eq!(id1, SchedulingContextId(1));

    // Free id0
    ctx_table.release(id0);
    assert!(!ctx_table.is_allocated(id0));

    // Next create reuses id0
    let id2 = ctx_table.create(3_000_000, 30_000_000);
    assert_eq!(id2, SchedulingContextId(0), "ID 0 reused from free list");
    assert!(ctx_table.is_allocated(id2));
}

// ============================================================
// Mixed handle lifecycle: full process exit cleanup
// ============================================================

#[test]
fn process_exit_drains_all_handle_types() {
    // Process holds one handle of each type — drain categorizes and cleans all
    let mut table = HandleTable::new();

    table
        .insert(HandleObject::Channel(ChannelId(0)), Rights::READ_WRITE)
        .unwrap();
    table
        .insert(HandleObject::Timer(TimerId(0)), Rights::READ)
        .unwrap();
    table
        .insert(HandleObject::Interrupt(InterruptId(0)), Rights::READ_WRITE)
        .unwrap();
    table
        .insert(HandleObject::Thread(ThreadId(1)), Rights::READ)
        .unwrap();
    table
        .insert(HandleObject::Process(ProcessId(2)), Rights::READ_WRITE)
        .unwrap();
    table
        .insert(
            HandleObject::SchedulingContext(SchedulingContextId(0)),
            Rights::READ_WRITE,
        )
        .unwrap();

    let drained = table.drain();
    assert_eq!(drained.len(), 6);

    let cats = categorize_handles(drained);
    assert_eq!(cats.channels.len(), 1);
    assert_eq!(cats.timers.len(), 1);
    assert_eq!(cats.interrupts.len(), 1);
    assert_eq!(cats.thread_handles.len(), 1);
    assert_eq!(cats.process_handles.len(), 1);
    assert_eq!(cats.scheduling_contexts.len(), 1);

    assert_eq!(table.count(), 0, "table fully drained");
}

#[test]
fn handle_send_rollback_restores_source() {
    // If handle_send phase 2 fails, the handle is restored to the source table
    let mut source = HandleTable::new();
    let mut target = HandleTable::new();

    // Fill target
    for i in 0..256u32 {
        target
            .insert(HandleObject::Channel(ChannelId(i + 100)), Rights::READ)
            .unwrap();
    }

    let h = source
        .insert(HandleObject::Timer(TimerId(5)), Rights::READ)
        .unwrap();

    // Phase 1: move from source
    let (obj, rights) = source.close(h).unwrap();
    assert_eq!(source.count(), 0);

    // Phase 2: insert into target fails
    let result = target.insert(obj, rights);
    assert!(result.is_none());

    // Rollback: restore to source at original slot
    assert!(source.insert_at(h, obj, rights));
    assert_eq!(source.count(), 1);

    // Verify the handle is back
    let slot = source.close(h).unwrap();
    assert!(matches!(slot.0, HandleObject::Timer(TimerId(5))));
}

#[test]
fn kill_process_releases_all_thread_context_refs() {
    // When process is killed, all threads' scheduling context refs are released
    let mut ctx_table = SchedulingContextTable::new();

    let ctx_id = ctx_table.create(1_000_000, 10_000_000);
    assert_eq!(ctx_table.ref_count(ctx_id), 1);

    // Simulate 3 threads bound to this context (each bind increments ref)
    ctx_table.increment_ref(ctx_id); // thread 1 bind
    ctx_table.increment_ref(ctx_id); // thread 2 bind
    ctx_table.increment_ref(ctx_id); // thread 3 bind
    assert_eq!(ctx_table.ref_count(ctx_id), 4); // 1 (handle) + 3 (binds)

    // One thread also has a saved_context_id (borrowed another)
    let other_ctx = ctx_table.create(500_000, 5_000_000);
    ctx_table.increment_ref(other_ctx); // borrow ref
    assert_eq!(ctx_table.ref_count(other_ctx), 2);

    // Kill: release all thread context refs
    ctx_table.release(ctx_id); // thread 1 bind
    ctx_table.release(ctx_id); // thread 2 bind
    ctx_table.release(ctx_id); // thread 3 bind
    ctx_table.release(other_ctx); // thread 1 saved (borrow)

    assert_eq!(ctx_table.ref_count(ctx_id), 1, "only handle ref remains");
    assert_eq!(ctx_table.ref_count(other_ctx), 1, "only handle ref remains");

    // Handle drain releases handle refs
    ctx_table.release(ctx_id); // handle close
    ctx_table.release(other_ctx); // handle close

    assert!(!ctx_table.is_allocated(ctx_id), "fully freed");
    assert!(!ctx_table.is_allocated(other_ctx), "fully freed");
}

#[test]
fn notify_on_destroyed_entry_is_noop() {
    // After handle close destroys exit notification, notify_exit is a no-op
    let mut reg = WaitableRegistry::new();
    let tid = ThreadId(42);

    reg.create(tid.0 as usize);
    reg.destroy(tid.0 as usize); // handle closed

    // Thread exits later — notify finds no entry
    let waiter = reg.notify(tid.0 as usize);
    assert!(waiter.is_none(), "notify on destroyed entry must be no-op");
}

#[test]
fn destroy_on_nonexistent_entry_is_noop() {
    // Destroying an entry that was never created is safe
    let mut reg = WaitableRegistry::new();

    let waiter = reg.destroy(999);
    assert!(waiter.is_none());
}

#[test]
fn sched_ctx_saturating_sub_prevents_underflow() {
    // release on already-freed context doesn't underflow
    let mut ctx_table = SchedulingContextTable::new();
    let ctx_id = ctx_table.create(1_000_000, 10_000_000);

    ctx_table.release(ctx_id); // ref 1→0, freed
    assert!(!ctx_table.is_allocated(ctx_id));

    // Extra release — slot is None, should be no-op
    ctx_table.release(ctx_id);
    // No panic, no underflow
}
