//! Interrupt-to-Event bridge — routes hardware IRQs to Event signal bits.
//!
//! Device interrupts (SPI, INTID >= 32) are bound to Events via `irq_bind`.
//! When an IRQ fires, the exception handler looks up the binding, signals the
//! event, and masks the interrupt at the GIC redistributor. The driver calls
//! `irq_ack` after processing, which clears the ack-pending flag and unmasks
//! the interrupt.
//!
//! Timer (INTID 27) and SGI/PPI interrupts (0-31) are kernel-internal and
//! cannot be bound to userspace events.

use crate::{
    config,
    types::{EventId, SyscallError},
};

pub const DEVICE_IRQ_BASE: u32 = 32;

#[derive(Debug, Clone, Copy)]
struct IrqBinding {
    event_id: EventId,
    signal_bits: u64,
    ack_pending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqSignal {
    pub event_id: EventId,
    pub signal_bits: u64,
}

pub struct IrqTable {
    bindings: [Option<IrqBinding>; config::MAX_IRQS],
}

#[allow(clippy::new_without_default)]
impl IrqTable {
    pub fn new() -> Self {
        IrqTable {
            bindings: [None; config::MAX_IRQS],
        }
    }

    pub fn bind(
        &mut self,
        intid: u32,
        event_id: EventId,
        signal_bits: u64,
    ) -> Result<(), SyscallError> {
        if intid < DEVICE_IRQ_BASE || (intid as usize) >= config::MAX_IRQS {
            return Err(SyscallError::InvalidArgument);
        }
        if signal_bits == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        let slot = &mut self.bindings[intid as usize];
        if slot.is_some() {
            return Err(SyscallError::InvalidArgument);
        }

        *slot = Some(IrqBinding {
            event_id,
            signal_bits,
            ack_pending: false,
        });

        Ok(())
    }

    pub fn unbind(&mut self, intid: u32) -> Result<(), SyscallError> {
        if intid < DEVICE_IRQ_BASE || (intid as usize) >= config::MAX_IRQS {
            return Err(SyscallError::InvalidArgument);
        }

        let slot = &mut self.bindings[intid as usize];

        if slot.is_none() {
            return Err(SyscallError::NotFound);
        }

        *slot = None;

        Ok(())
    }

    /// Look up a binding for an INTID and mark it ack-pending.
    /// Returns the event to signal, or None if no binding exists.
    pub fn handle_irq(&mut self, intid: u32) -> Option<IrqSignal> {
        let binding = self.bindings.get_mut(intid as usize)?.as_mut()?;

        binding.ack_pending = true;

        Some(IrqSignal {
            event_id: binding.event_id,
            signal_bits: binding.signal_bits,
        })
    }

    /// Acknowledge a handled IRQ, clearing the ack-pending flag.
    pub fn ack(&mut self, intid: u32) -> Result<(), SyscallError> {
        if intid < DEVICE_IRQ_BASE || (intid as usize) >= config::MAX_IRQS {
            return Err(SyscallError::InvalidArgument);
        }

        let binding = self.bindings[intid as usize]
            .as_mut()
            .ok_or(SyscallError::NotFound)?;

        if !binding.ack_pending {
            return Err(SyscallError::InvalidArgument);
        }

        binding.ack_pending = false;

        Ok(())
    }

    /// Return INTIDs bound to a given event whose signal_bits overlap
    /// with `cleared_bits`. Used by event_clear to auto-unmask IRQs.
    pub fn intids_for_event_bits(&self, event_id: EventId, cleared_bits: u64) -> ([u32; 4], usize) {
        let mut result = [0u32; 4];
        let mut count = 0;

        for (intid, slot) in self.bindings.iter().enumerate() {
            if let Some(b) = slot
                && b.event_id == event_id
                && b.signal_bits & cleared_bits != 0
                && count < 4
            {
                result[count] = intid as u32;
                count += 1;
            }
        }

        (result, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_INTID: u32 = 64;

    fn make_table() -> IrqTable {
        IrqTable::new()
    }

    // -- Bind --

    #[test]
    fn bind_valid_device_irq() {
        let mut t = make_table();

        assert!(t.bind(TEST_INTID, EventId(0), 0b1).is_ok());
    }

    #[test]
    fn bind_rejects_sgi_ppi_range() {
        let mut t = make_table();

        for intid in 0..DEVICE_IRQ_BASE {
            assert_eq!(
                t.bind(intid, EventId(0), 0b1),
                Err(SyscallError::InvalidArgument)
            );
        }
    }

    #[test]
    fn bind_rejects_out_of_range() {
        let mut t = make_table();

        assert_eq!(
            t.bind(config::MAX_IRQS as u32, EventId(0), 0b1),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn bind_rejects_zero_signal_bits() {
        let mut t = make_table();

        assert_eq!(
            t.bind(TEST_INTID, EventId(0), 0),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn bind_rejects_double_bind() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();

        assert_eq!(
            t.bind(TEST_INTID, EventId(1), 0b1),
            Err(SyscallError::InvalidArgument)
        );
    }

    #[test]
    fn bind_first_valid_spi() {
        let mut t = make_table();

        assert!(t.bind(DEVICE_IRQ_BASE, EventId(0), 0b1).is_ok());
    }

    #[test]
    fn bind_last_valid_intid() {
        let mut t = make_table();

        assert!(
            t.bind((config::MAX_IRQS - 1) as u32, EventId(0), 0b1)
                .is_ok()
        );
    }

    // -- Unbind --

    #[test]
    fn unbind_existing() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();

        assert!(t.unbind(TEST_INTID).is_ok());
        assert!(t.handle_irq(TEST_INTID).is_none());
    }

    #[test]
    fn unbind_not_found() {
        let mut t = make_table();

        assert_eq!(t.unbind(TEST_INTID), Err(SyscallError::NotFound));
    }

    #[test]
    fn unbind_allows_rebind() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();
        t.unbind(TEST_INTID).unwrap();

        assert!(t.bind(TEST_INTID, EventId(1), 0b10).is_ok());
    }

    // -- Handle IRQ --

    #[test]
    fn handle_irq_returns_signal() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(7), 0xFF).unwrap();

        let sig = t.handle_irq(TEST_INTID).unwrap();

        assert_eq!(sig.event_id, EventId(7));
        assert_eq!(sig.signal_bits, 0xFF);
    }

    #[test]
    fn handle_irq_returns_none_for_unbound() {
        let mut t = make_table();

        assert!(t.handle_irq(TEST_INTID).is_none());
    }

    #[test]
    fn handle_irq_sets_ack_pending() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();
        t.handle_irq(TEST_INTID).unwrap();

        assert!(t.ack(TEST_INTID).is_ok());
    }

    // -- Ack --

    #[test]
    fn ack_clears_pending() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();
        t.handle_irq(TEST_INTID).unwrap();
        t.ack(TEST_INTID).unwrap();

        assert_eq!(t.ack(TEST_INTID), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn ack_without_pending_fails() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(0), 0b1).unwrap();

        assert_eq!(t.ack(TEST_INTID), Err(SyscallError::InvalidArgument));
    }

    #[test]
    fn ack_unbound_fails() {
        let mut t = make_table();

        assert_eq!(t.ack(TEST_INTID), Err(SyscallError::NotFound));
    }

    // -- Full flow --

    #[test]
    fn bind_handle_ack_cycle() {
        let mut t = make_table();

        t.bind(TEST_INTID, EventId(3), 0b1010).unwrap();

        let sig = t.handle_irq(TEST_INTID).unwrap();

        assert_eq!(sig.event_id, EventId(3));
        assert_eq!(sig.signal_bits, 0b1010);

        t.ack(TEST_INTID).unwrap();

        let sig2 = t.handle_irq(TEST_INTID).unwrap();

        assert_eq!(sig2, sig);

        t.ack(TEST_INTID).unwrap();
    }

    #[test]
    fn multiple_bindings_independent() {
        let mut t = make_table();

        t.bind(64, EventId(0), 0b01).unwrap();
        t.bind(65, EventId(1), 0b10).unwrap();

        let s0 = t.handle_irq(64).unwrap();
        let s1 = t.handle_irq(65).unwrap();

        assert_eq!(s0.event_id, EventId(0));
        assert_eq!(s1.event_id, EventId(1));

        t.ack(64).unwrap();

        assert_eq!(t.ack(65), Ok(()));
    }
}
