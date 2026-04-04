// AUDIT: 2026-03-11 — 0 unsafe blocks (pure safe Rust). 6-category checklist
// applied. Findings: none — module is sound. Two-phase wake pattern in
// handle_irq correctly collects wakeups under interrupt table lock, then wakes
// via scheduler after release. Lock ordering (interrupt → scheduler) maintained.
// Edge-triggered semantics correctly implemented: pending set on IRQ fire,
// cleared by interrupt_ack. Registration rejects duplicates. Destruction masks
// IRQ and wakes blocked driver. MAX_INTERRUPTS=32 matches GIC SPI range for
// typical QEMU virt configurations.
//
//! Interrupt forwarding to userspace.
//!
//! Enables the microkernel driver model: device drivers run at EL0, accessing
//! hardware via MMIO mappings and receiving interrupts through waitable handles.
//!
//! # Lifecycle
//!
//! 1. Process calls `interrupt_register(irq)` — kernel enables the IRQ in the
//!    GIC and returns a handle.
//! 2. Process calls `wait([handle], ...)` — blocks until the IRQ fires.
//! 3. IRQ fires → kernel masks the IRQ at the GIC distributor (prevents
//!    re-triggering), marks the handle as pending, wakes the driver thread.
//! 4. Driver processes the interrupt (MMIO reads/writes — zero overhead).
//! 5. Driver calls `interrupt_ack(handle)` — kernel clears pending, unmasks
//!    the IRQ. Ready for the next interrupt.
//!
//! # Edge vs level triggered
//!
//! The handle uses edge semantics: pending is set on each IRQ and consumed by
//! `interrupt_ack`. This differs from timer handles (level-triggered, permanently
//! ready). A driver that misses an ack will see pending=true on the next wait.
//!
//! Waiter registration and readiness tracking are delegated to `WaitableRegistry`.

use super::{
    handle::HandleObject,
    interrupt_controller::{self, InterruptController},
    scheduler,
    sync::IrqMutex,
    thread::ThreadId,
    waitable::{WaitableId, WaitableRegistry},
};

/// Maximum concurrent registered interrupts across all processes (from system_config via paging).
const MAX_INTERRUPTS: usize = super::paging::MAX_INTERRUPTS as usize;

static TABLE: IrqMutex<InterruptTable> = IrqMutex::new(InterruptTable {
    slots: [const { None }; MAX_INTERRUPTS],
    waiters: WaitableRegistry::new(),
});

struct InterruptTable {
    /// IRQ number for each slot. `None` = free slot.
    slots: [Option<u32>; MAX_INTERRUPTS],
    /// Readiness + waiter tracking for each interrupt.
    waiters: WaitableRegistry<InterruptId>,
}

/// Opaque interrupt identifier. Index into the global interrupt table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptId(pub u8);

impl WaitableId for InterruptId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

/// Acknowledge an interrupt (called from `interrupt_ack` syscall).
///
/// Clears the pending flag and re-enables the IRQ in the GIC distributor,
/// allowing the device to trigger the next interrupt.
pub fn acknowledge(id: InterruptId) {
    let mut table = TABLE.lock();

    table.waiters.clear_ready(id);

    if let Some(&irq) = table.slots[id.0 as usize].as_ref() {
        interrupt_controller::GIC.enable_irq(irq);
    }
}
/// Check whether an interrupt is pending (for `sys_wait` readiness check).
///
/// Edge-triggered: returns true only between IRQ fire and `interrupt_ack`.
pub fn check_pending(id: InterruptId) -> bool {
    TABLE.lock().waiters.check_ready(id)
}
/// Destroy an interrupt registration (called from `handle_close`).
///
/// Disables the IRQ in the GIC and wakes any thread blocked on this handle.
pub fn destroy(id: InterruptId) {
    let (irq, waiter) = {
        let mut table = TABLE.lock();
        let irq = table.slots[id.0 as usize].take();
        let waiter = table.waiters.destroy(id);

        (irq, waiter)
    };

    if let Some(irq) = irq {
        interrupt_controller::GIC.disable_irq(irq);
    }

    if let Some(waiter_id) = waiter {
        scheduler::wake_for_handle(waiter_id, HandleObject::Interrupt(id));
    }
}
/// Handle an IRQ from the hardware. Called from `irq_handler` in main.rs.
///
/// If the IRQ is registered for forwarding: masks it in the GIC distributor,
/// marks the handle as pending, and wakes the waiting driver thread.
///
/// Two-phase wake: collect the waiter under the interrupt table lock, then
/// wake via the scheduler after releasing it. Maintains lock ordering:
/// interrupt → scheduler (same direction as channel/timer → scheduler).
///
/// Returns true if this IRQ was handled (registered for forwarding).
pub fn handle_irq(irq: u32) -> bool {
    let mut found = false;
    let mut to_wake: Option<(InterruptId, ThreadId)> = None;

    {
        let mut table = TABLE.lock();

        for i in 0..MAX_INTERRUPTS {
            if table.slots[i] == Some(irq) {
                found = true;

                let id = InterruptId(i as u8);

                // Mask the IRQ until the driver acknowledges.
                interrupt_controller::GIC.disable_irq(irq);

                if let Some(waiter) = table.waiters.notify(id) {
                    to_wake = Some((id, waiter));
                }

                break; // Only one registration per IRQ.
            }
        }
    }

    // Phase 2: wake the driver thread (acquires scheduler lock).
    if let Some((int_id, thread_id)) = to_wake {
        scheduler::wake_for_handle(thread_id, HandleObject::Interrupt(int_id));
    }

    found
}
/// Register for an IRQ. Enables the IRQ in the GIC distributor.
///
/// Returns the interrupt ID on success, or `None` if the table is full
/// or the IRQ is already registered.
pub fn register(irq: u32) -> Option<InterruptId> {
    let mut table = TABLE.lock();

    // Reject duplicate registration.
    for slot in table.slots.iter() {
        if *slot == Some(irq) {
            return None;
        }
    }

    // Find a free slot.
    for i in 0..MAX_INTERRUPTS {
        if table.slots[i].is_none() {
            let id = InterruptId(i as u8);

            table.slots[i] = Some(irq);
            table.waiters.create(id);

            // Enable the IRQ in the GIC so the hardware delivers it to us.
            interrupt_controller::GIC.enable_irq(irq);

            return Some(id);
        }
    }

    None
}
/// Register a thread as the waiter for this interrupt.
///
/// Called by `sys_wait` before checking readiness. If the IRQ fires between
/// registration and blocking, the wake is delivered correctly.
pub fn register_waiter(id: InterruptId, thread: ThreadId) {
    TABLE.lock().waiters.register_waiter(id, thread);
}
/// Unregister a thread from an interrupt (cleanup when `wait` returns).
///
/// Safe to call even if the waiter was already cleared by the fire path.
pub fn unregister_waiter(id: InterruptId) {
    TABLE.lock().waiters.unregister_waiter(id);
}
