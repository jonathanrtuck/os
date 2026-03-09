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

use super::handle::HandleObject;
use super::interrupt_controller;
use super::scheduler;
use super::sync::IrqMutex;
use super::thread::ThreadId;

/// A registered interrupt.
struct Interrupt {
    /// GIC IRQ number.
    irq: u32,
    /// True when the IRQ has fired and hasn't been acknowledged.
    pending: bool,
    /// Thread currently waiting on this interrupt via `wait`.
    waiter: Option<ThreadId>,
}
struct InterruptTable {
    slots: [Option<Interrupt>; MAX_INTERRUPTS],
}

/// Opaque interrupt identifier. Index into the global interrupt table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptId(pub u8);

/// Maximum concurrent registered interrupts across all processes.
const MAX_INTERRUPTS: usize = 32;

static TABLE: IrqMutex<InterruptTable> = IrqMutex::new(InterruptTable {
    slots: [const { None }; MAX_INTERRUPTS],
});

/// Acknowledge an interrupt (called from `interrupt_ack` syscall).
///
/// Clears the pending flag and re-enables the IRQ in the GIC distributor,
/// allowing the device to trigger the next interrupt.
pub fn acknowledge(id: InterruptId) {
    let mut table = TABLE.lock();

    if let Some(int) = &mut table.slots[id.0 as usize] {
        int.pending = false;

        interrupt_controller::enable_irq(int.irq);
    }
}
/// Check whether an interrupt is pending (for `sys_wait` readiness check).
///
/// Edge-triggered: returns true only between IRQ fire and `interrupt_ack`.
pub fn check_pending(id: InterruptId) -> bool {
    let table = TABLE.lock();

    table.slots[id.0 as usize]
        .as_ref()
        .is_some_and(|int| int.pending)
}
/// Destroy an interrupt registration (called from `handle_close`).
///
/// Disables the IRQ in the GIC — no handler will process it after this.
pub fn destroy(id: InterruptId) {
    let mut table = TABLE.lock();

    if let Some(int) = table.slots[id.0 as usize].take() {
        interrupt_controller::disable_irq(int.irq);
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

        for (i, slot) in table.slots.iter_mut().enumerate() {
            if let Some(int) = slot {
                if int.irq == irq {
                    found = true;
                    int.pending = true;

                    // Mask the IRQ until the driver acknowledges.
                    interrupt_controller::disable_irq(irq);

                    if let Some(waiter) = int.waiter.take() {
                        to_wake = Some((InterruptId(i as u8), waiter));
                    }

                    break; // Only one registration per IRQ.
                }
            }
        }
    }

    // Phase 2: wake the driver thread (acquires scheduler lock).
    if let Some((int_id, thread_id)) = to_wake {
        if !scheduler::try_wake_for_handle(thread_id, HandleObject::Interrupt(int_id)) {
            scheduler::set_wake_pending_for_handle(thread_id, HandleObject::Interrupt(int_id));
        }
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
        if let Some(int) = slot {
            if int.irq == irq {
                return None;
            }
        }
    }

    // Find a free slot.
    for (i, slot) in table.slots.iter_mut().enumerate() {
        if slot.is_none() {
            *slot = Some(Interrupt {
                irq,
                pending: false,
                waiter: None,
            });

            // Enable the IRQ in the GIC so the hardware delivers it to us.
            interrupt_controller::enable_irq(irq);

            return Some(InterruptId(i as u8));
        }
    }

    None
}
/// Register a thread as the waiter for this interrupt.
///
/// Called by `sys_wait` before checking readiness. If the IRQ fires between
/// registration and blocking, the wake is delivered correctly.
pub fn register_waiter(id: InterruptId, thread: ThreadId) {
    let mut table = TABLE.lock();

    if let Some(int) = &mut table.slots[id.0 as usize] {
        int.waiter = Some(thread);
    }
}
/// Unregister a thread from an interrupt (cleanup when `wait` returns).
///
/// Safe to call even if the waiter was already cleared by the fire path.
pub fn unregister_waiter(id: InterruptId) {
    let mut table = TABLE.lock();

    if let Some(int) = &mut table.slots[id.0 as usize] {
        int.waiter = None;
    }
}
