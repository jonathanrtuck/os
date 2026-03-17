# GICv3 Known Traps

## ICC_SRE_EL2.Enable Must Be Set in boot.S

When transitioning from EL2 to EL1, `ICC_SRE_EL2.Enable` (bit 3) must be set to 1 **before** dropping to EL1. Without this, EL1 writes to `ICC_SRE_EL1` are trapped or silently ignored, and the GICv3 CPU interface never activates. The system register interface (`ICC_IAR1_EL1`, `ICC_EOIR1_EL1`, etc.) will appear to work but interrupts never arrive.

**Where:** `system/kernel/boot.S`, in the EL2→EL1 transition sequence.

## IGROUPR / GICR_IGROUPR0 Must Be Group 1 NS

All interrupts (SGIs, PPIs, SPIs) must be configured as **Group 1 Non-Secure** for delivery via `ICC_IAR1_EL1`. This means:
- `GICR_IGROUPR0` = 0xFFFF_FFFF (all SGIs + PPIs in Group 1 NS) — set per-core in GICR init
- `GICD_IGROUPR[n]` = 0xFFFF_FFFF (all SPIs in Group 1 NS) — set in distributor init

Without this, interrupts are routed to Group 0 / FIQ / EL3 and never reach the kernel's IRQ handler. The system boots but no timer ticks, no device interrupts — complete silence.

**Where:** `system/kernel/interrupt_controller.rs`, in `init_per_core()` (GICR) and `init_distributor()` (GICD).
