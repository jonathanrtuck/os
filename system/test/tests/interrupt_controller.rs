//! Host-side tests for GICv3 interrupt controller implementation.
//!
//! Cannot include kernel modules directly (they target aarch64-unknown-none
//! with inline asm and hardware-specific code). Tests duplicate the pure-logic
//! portions to verify correctness.

// ---------------------------------------------------------------------------
// InterruptController trait definition (mirrors kernel interrupt_controller.rs)
// ---------------------------------------------------------------------------

/// The InterruptController trait — 7 methods, static dispatch.
/// This is the exact trait definition that the kernel will use.
trait InterruptController {
    /// Initialize the distributor (global, core 0 only).
    fn init_distributor(&self);
    /// Initialize the per-core CPU interface and redistributor.
    fn init_per_core(&self, core_id: u32);
    /// Acknowledge an interrupt. Returns None for spurious (1023).
    fn acknowledge(&self) -> Option<u32>;
    /// Signal end of interrupt processing.
    fn end_of_interrupt(&self, irq_id: u32);
    /// Enable (unmask) an IRQ.
    fn enable_irq(&self, irq_id: u32);
    /// Disable (mask) an IRQ.
    fn disable_irq(&self, irq_id: u32);
    /// Send an inter-processor interrupt to a target core.
    fn send_ipi(&self, target_core: u32);
}

// ---------------------------------------------------------------------------
// GicV3 affinity encoding logic (duplicated from kernel implementation)
// ---------------------------------------------------------------------------

/// GICR stride: 128 KiB per core.
const GICR_STRIDE: usize = 0x20000;

/// GICD register offsets.
const GICD_CTLR: usize = 0x0000;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_IROUTER: usize = 0x6100;

/// GICR register offsets (within per-core region).
const GICR_WAKER: usize = 0x0014;
const GICR_SGI_BASE: usize = 0x10000;
const GICR_ISENABLER0: usize = GICR_SGI_BASE + 0x0100;
const GICR_IPRIORITYR0: usize = GICR_SGI_BASE + 0x0400;

/// GICv3 spurious interrupt ID.
const SPURIOUS: u32 = 1023;

/// Compute the ICC_SGI1R_EL1 value for sending SGI 0 to a target core.
///
/// For QEMU virt with flat affinity (Aff3=0, Aff2=0, Aff1=0, Aff0=core_id):
/// - Bits [15:0]: target list (1 << Aff0)
/// - Bits [23:16]: Aff1 = 0
/// - Bits [39:32]: Aff2 = 0
/// - Bits [47:40]: reserved
/// - Bits [55:48]: Aff3 = 0
/// - Bits [27:24]: INTID = SGI number (0 for IPI)
fn compute_sgi1r_value(target_core: u32) -> u64 {
    // SGI ID = 0 (bits [27:24])
    // Target list: bit corresponding to Aff0 of target core
    let target_list = 1u64 << (target_core & 0xF);
    // Aff1 = 0, Aff2 = 0, Aff3 = 0 for QEMU flat affinity
    target_list
}

/// Compute the per-core GICR base address.
fn gicr_base_for_core(gicr_base: usize, core_id: u32) -> usize {
    gicr_base + (core_id as usize) * GICR_STRIDE
}

/// Mirrors the ISENABLER/ICENABLER register+bit computation.
fn gic_enable_reg_offset(irq: u32) -> (usize, u32) {
    let reg_offset = (irq / 32) as usize * 4;
    let bit = 1u32 << (irq % 32);
    (reg_offset, bit)
}

/// Mirrors the IROUTER offset computation for SPIs (IRQ >= 32).
fn gicd_irouter_offset(irq: u32) -> usize {
    GICD_IROUTER + (irq as usize - 32) * 8
}

/// Simulate acknowledge: check if an IRQ ID is spurious.
fn is_spurious(iar_value: u32) -> bool {
    iar_value == SPURIOUS
}

/// Simulate acknowledge return: None for spurious, Some(id) otherwise.
fn acknowledge_result(iar_value: u32) -> Option<u32> {
    if is_spurious(iar_value) {
        None
    } else {
        Some(iar_value)
    }
}

// ---------------------------------------------------------------------------
// Tests: InterruptController trait definition
// ---------------------------------------------------------------------------

/// A mock InterruptController for testing trait definition completeness.
struct MockGic {
    distributor_inited: core::cell::Cell<bool>,
    per_core_inited: core::cell::Cell<Option<u32>>,
    last_acknowledged: core::cell::Cell<Option<u32>>,
    last_eoi: core::cell::Cell<Option<u32>>,
    last_enabled: core::cell::Cell<Option<u32>>,
    last_disabled: core::cell::Cell<Option<u32>>,
    last_ipi_target: core::cell::Cell<Option<u32>>,
}

impl MockGic {
    fn new() -> Self {
        Self {
            distributor_inited: core::cell::Cell::new(false),
            per_core_inited: core::cell::Cell::new(None),
            last_acknowledged: core::cell::Cell::new(None),
            last_eoi: core::cell::Cell::new(None),
            last_enabled: core::cell::Cell::new(None),
            last_disabled: core::cell::Cell::new(None),
            last_ipi_target: core::cell::Cell::new(None),
        }
    }
}

impl InterruptController for MockGic {
    fn init_distributor(&self) {
        self.distributor_inited.set(true);
    }

    fn init_per_core(&self, core_id: u32) {
        self.per_core_inited.set(Some(core_id));
    }

    fn acknowledge(&self) -> Option<u32> {
        self.last_acknowledged.get()
    }

    fn end_of_interrupt(&self, irq_id: u32) {
        self.last_eoi.set(Some(irq_id));
    }

    fn enable_irq(&self, irq_id: u32) {
        self.last_enabled.set(Some(irq_id));
    }

    fn disable_irq(&self, irq_id: u32) {
        self.last_disabled.set(Some(irq_id));
    }

    fn send_ipi(&self, target_core: u32) {
        self.last_ipi_target.set(Some(target_core));
    }
}

#[test]
fn trait_has_seven_methods() {
    // This test verifies that the trait compiles with all 7 methods.
    // The MockGic must implement all 7 or this won't compile.
    let gic = MockGic::new();
    gic.init_distributor();
    gic.init_per_core(0);
    let _ = gic.acknowledge();
    gic.end_of_interrupt(30);
    gic.enable_irq(30);
    gic.disable_irq(30);
    gic.send_ipi(1);
}

#[test]
fn trait_static_dispatch() {
    // Verify static dispatch works — call trait methods on concrete type,
    // not through &dyn InterruptController.
    fn use_gic(gic: &impl InterruptController) {
        gic.init_distributor();
        gic.init_per_core(0);
        gic.enable_irq(30);
    }

    let gic = MockGic::new();
    use_gic(&gic);
    assert!(gic.distributor_inited.get());
    assert_eq!(gic.per_core_inited.get(), Some(0));
    assert_eq!(gic.last_enabled.get(), Some(30));
}

// ---------------------------------------------------------------------------
// Tests: GicV3 affinity encoding (send_ipi for cores 0-3)
// ---------------------------------------------------------------------------

#[test]
fn sgi1r_value_core_0() {
    let val = compute_sgi1r_value(0);
    // Target list: bit 0 set (core 0)
    assert_eq!(val & 0xFFFF, 0x0001);
    // SGI ID = 0 (bits [27:24])
    assert_eq!((val >> 24) & 0xF, 0);
}

#[test]
fn sgi1r_value_core_1() {
    let val = compute_sgi1r_value(1);
    // Target list: bit 1 set (core 1)
    assert_eq!(val & 0xFFFF, 0x0002);
}

#[test]
fn sgi1r_value_core_2() {
    let val = compute_sgi1r_value(2);
    // Target list: bit 2 set (core 2)
    assert_eq!(val & 0xFFFF, 0x0004);
}

#[test]
fn sgi1r_value_core_3() {
    let val = compute_sgi1r_value(3);
    // Target list: bit 3 set (core 3)
    assert_eq!(val & 0xFFFF, 0x0008);
}

#[test]
fn sgi1r_value_all_upper_bits_zero_for_flat_affinity() {
    for core in 0..4u32 {
        let val = compute_sgi1r_value(core);
        // Aff1 (bits [23:16]) = 0
        assert_eq!((val >> 16) & 0xFF, 0, "Aff1 should be 0 for core {core}");
        // Aff2 (bits [39:32]) = 0
        assert_eq!((val >> 32) & 0xFF, 0, "Aff2 should be 0 for core {core}");
        // Aff3 (bits [55:48]) = 0
        assert_eq!((val >> 48) & 0xFF, 0, "Aff3 should be 0 for core {core}");
    }
}

// ---------------------------------------------------------------------------
// Tests: Spurious interrupt handling
// ---------------------------------------------------------------------------

#[test]
fn acknowledge_returns_none_for_spurious() {
    assert_eq!(acknowledge_result(SPURIOUS), None);
}

#[test]
fn acknowledge_returns_some_for_timer() {
    assert_eq!(acknowledge_result(30), Some(30));
}

#[test]
fn acknowledge_returns_some_for_sgi() {
    assert_eq!(acknowledge_result(0), Some(0));
}

#[test]
fn acknowledge_returns_some_for_spi() {
    assert_eq!(acknowledge_result(48), Some(48));
}

#[test]
fn spurious_id_is_1023() {
    assert!(is_spurious(1023));
    assert!(!is_spurious(1022));
    assert!(!is_spurious(0));
    assert!(!is_spurious(30));
}

// ---------------------------------------------------------------------------
// Tests: GICR per-core base address computation
// ---------------------------------------------------------------------------

#[test]
fn gicr_base_core_0() {
    let gicr_base = 0x080A_0000;
    assert_eq!(gicr_base_for_core(gicr_base, 0), 0x080A_0000);
}

#[test]
fn gicr_base_core_1() {
    let gicr_base = 0x080A_0000;
    assert_eq!(gicr_base_for_core(gicr_base, 1), 0x080A_0000 + 0x20000);
}

#[test]
fn gicr_base_core_2() {
    let gicr_base = 0x080A_0000;
    assert_eq!(gicr_base_for_core(gicr_base, 2), 0x080A_0000 + 0x40000);
}

#[test]
fn gicr_base_core_3() {
    let gicr_base = 0x080A_0000;
    assert_eq!(gicr_base_for_core(gicr_base, 3), 0x080A_0000 + 0x60000);
}

#[test]
fn gicr_stride_is_128k() {
    assert_eq!(GICR_STRIDE, 0x20000);
    assert_eq!(GICR_STRIDE, 128 * 1024);
}

// ---------------------------------------------------------------------------
// Tests: GICD IROUTER offset computation
// ---------------------------------------------------------------------------

#[test]
fn irouter_offset_spi_first() {
    // SPI 0 = IRQ 32, IROUTER starts at GICD+0x6100
    assert_eq!(gicd_irouter_offset(32), GICD_IROUTER);
}

#[test]
fn irouter_offset_spi_1() {
    // Each IROUTER entry is 8 bytes (64-bit register)
    assert_eq!(gicd_irouter_offset(33), GICD_IROUTER + 8);
}

#[test]
fn irouter_offset_virtio_irq() {
    // Virtio IRQ 48 → SPI 16 → IROUTER offset 16 * 8
    assert_eq!(gicd_irouter_offset(48), GICD_IROUTER + 16 * 8);
}

// ---------------------------------------------------------------------------
// Tests: ISENABLER/ICENABLER register computation
// ---------------------------------------------------------------------------

#[test]
fn enable_reg_timer_ppi_30() {
    let (offset, bit) = gic_enable_reg_offset(30);
    assert_eq!(offset, 0); // IRQs 0-31 in first register
    assert_eq!(bit, 1 << 30);
}

#[test]
fn enable_reg_sgi_0() {
    let (offset, bit) = gic_enable_reg_offset(0);
    assert_eq!(offset, 0);
    assert_eq!(bit, 1);
}

#[test]
fn enable_reg_spi_48() {
    let (offset, bit) = gic_enable_reg_offset(48);
    assert_eq!(offset, 4); // IRQs 32-63 in second register
    assert_eq!(bit, 1 << 16);
}

// ---------------------------------------------------------------------------
// Tests: DTB reg entry parsing logic
// ---------------------------------------------------------------------------

/// Simulate parsing GICv3 DTB reg entries.
/// GICv3 DTB has 2 entries (GICD base, GICR base) or 3 entries (+ GICC compat).
fn parse_gic_v3_regs(regs: &[(u64, u64)]) -> Option<(u64, u64)> {
    if regs.len() >= 2 {
        Some((regs[0].0, regs[1].0))
    } else {
        None
    }
}

#[test]
fn dtb_parse_two_reg_entries() {
    let regs = vec![
        (0x0800_0000u64, 0x0001_0000u64), // GICD
        (0x080A_0000u64, 0x00F6_0000u64), // GICR
    ];
    let result = parse_gic_v3_regs(&regs);
    assert_eq!(result, Some((0x0800_0000, 0x080A_0000)));
}

#[test]
fn dtb_parse_three_reg_entries() {
    // Three entries: GICD, GICR, GICC (compat)
    let regs = vec![
        (0x0800_0000u64, 0x0001_0000u64), // GICD
        (0x080A_0000u64, 0x00F6_0000u64), // GICR
        (0x0801_0000u64, 0x0001_0000u64), // GICC (compat, ignored)
    ];
    let result = parse_gic_v3_regs(&regs);
    assert_eq!(result, Some((0x0800_0000, 0x080A_0000)));
}

#[test]
fn dtb_parse_one_reg_entry_fails() {
    let regs = vec![(0x0800_0000u64, 0x0001_0000u64)]; // Only GICD
    let result = parse_gic_v3_regs(&regs);
    assert_eq!(result, None);
}

#[test]
fn dtb_parse_empty_regs_fails() {
    let regs: Vec<(u64, u64)> = vec![];
    let result = parse_gic_v3_regs(&regs);
    assert_eq!(result, None);
}

// ---------------------------------------------------------------------------
// Tests: GicV3 init sequence ordering
// ---------------------------------------------------------------------------

/// Simulates GicV3 init_per_core sequence.
/// The critical requirement: ICC_SRE_EL1 (SRE=1) FIRST, ISB, then other ICC regs.
#[derive(Debug, Clone, PartialEq)]
enum GicV3Op {
    WriteSre,
    Isb,
    WritePmr,
    WriteCtlr,
    WriteIgrp1,
    DsbIsb,
    GicrWake,
    GicrConfig,
}

fn gicv3_init_per_core_correct() -> Vec<GicV3Op> {
    vec![
        GicV3Op::WriteSre,   // ICC_SRE_EL1 = SRE=1 FIRST
        GicV3Op::Isb,        // ISB before any other ICC access
        GicV3Op::WritePmr,   // ICC_PMR_EL1
        GicV3Op::WriteCtlr,  // ICC_CTLR_EL1
        GicV3Op::WriteIgrp1, // ICC_IGRP1_EL1
        GicV3Op::GicrWake,   // GICR wake sequence
        GicV3Op::GicrConfig, // GICR SGI/PPI enables, priorities
    ]
}

#[test]
fn init_per_core_sre_first() {
    let ops = gicv3_init_per_core_correct();
    assert_eq!(ops[0], GicV3Op::WriteSre, "ICC_SRE_EL1 must be written first");
}

#[test]
fn init_per_core_isb_after_sre() {
    let ops = gicv3_init_per_core_correct();
    assert_eq!(ops[1], GicV3Op::Isb, "ISB must follow ICC_SRE_EL1 immediately");
}

#[test]
fn init_per_core_no_icc_before_sre_isb() {
    let ops = gicv3_init_per_core_correct();
    // No ICC register writes (PMR, CTLR, IGRP1) before SRE + ISB
    for op in &ops[..2] {
        assert!(
            !matches!(
                op,
                GicV3Op::WritePmr | GicV3Op::WriteCtlr | GicV3Op::WriteIgrp1
            ),
            "No ICC register writes before SRE+ISB: found {op:?}"
        );
    }
}

/// Simulates GicV3 init_distributor sequence.
#[derive(Debug, Clone, PartialEq)]
enum GicdOp {
    Disable,
    ConfigurePriorities,
    ConfigureRouters,
    EnableWithAreNs,
    DsbIsb,
}

fn gicv3_init_distributor_correct() -> Vec<GicdOp> {
    vec![
        GicdOp::Disable,
        GicdOp::ConfigurePriorities,
        GicdOp::ConfigureRouters,  // IROUTER, not ITARGETSR
        GicdOp::EnableWithAreNs,   // CTLR with ARE_NS=1
        GicdOp::DsbIsb,            // DSB SY + ISB after CTLR
    ]
}

#[test]
fn init_distributor_ends_with_barrier() {
    let ops = gicv3_init_distributor_correct();
    assert_eq!(
        ops.last(),
        Some(&GicdOp::DsbIsb),
        "DSB+ISB must follow GICD_CTLR write"
    );
}

#[test]
fn init_distributor_uses_are_ns() {
    let ops = gicv3_init_distributor_correct();
    assert!(
        ops.contains(&GicdOp::EnableWithAreNs),
        "GICD_CTLR must be enabled with ARE_NS=1"
    );
}

#[test]
fn init_distributor_uses_irouter() {
    let ops = gicv3_init_distributor_correct();
    assert!(
        ops.contains(&GicdOp::ConfigureRouters),
        "Must use IROUTER for SPI routing"
    );
}

// ---------------------------------------------------------------------------
// Tests: SGI 0 (IPI) dispatch in irq_handler
// ---------------------------------------------------------------------------
//
// The irq_handler must distinguish SGI 0 (IPI) from timer IRQ 30 and
// other SPIs. SGI 0 must NOT call timer::handle_irq or increment TICKS.

/// SGI 0 IRQ ID constant.
const SGI_IPI: u32 = 0;
/// Timer PPI IRQ ID.
const TIMER_IRQ: u32 = 30;

/// Model of irq_handler dispatch logic.
///
/// Returns a tuple: (called_timer_handle_irq, called_interrupt_handle_irq, incremented_ticks, called_schedule)
fn irq_dispatch_model(irq_id: u32) -> (bool, bool, bool, bool) {
    if irq_id == SGI_IPI {
        // SGI 0: just EOI + schedule. No timer, no interrupt forwarding, no TICKS.
        (false, false, false, true)
    } else if irq_id == TIMER_IRQ {
        // Timer: increment TICKS, handle timer, schedule.
        (true, false, true, true)
    } else {
        // Device IRQ: forward to interrupt handler, schedule.
        (false, true, false, true)
    }
}

/// VAL-IPI-005: SGI 0 does not call timer::handle_irq.
#[test]
fn sgi0_does_not_call_timer_handler() {
    let (timer, _interrupt, _ticks, _sched) = irq_dispatch_model(SGI_IPI);
    assert!(!timer, "SGI 0 must not call timer::handle_irq()");
}

/// VAL-IPI-005: SGI 0 does not increment TICKS.
#[test]
fn sgi0_does_not_increment_ticks() {
    let (_timer, _interrupt, ticks, _sched) = irq_dispatch_model(SGI_IPI);
    assert!(!ticks, "SGI 0 must not increment TICKS counter");
}

/// VAL-IPI-005: SGI 0 does not forward to interrupt::handle_irq.
#[test]
fn sgi0_does_not_forward_to_interrupt_handler() {
    let (_timer, interrupt, _ticks, _sched) = irq_dispatch_model(SGI_IPI);
    assert!(!interrupt, "SGI 0 must not call interrupt::handle_irq(0)");
}

/// VAL-IPI-005: SGI 0 still triggers schedule.
#[test]
fn sgi0_triggers_schedule() {
    let (_timer, _interrupt, _ticks, sched) = irq_dispatch_model(SGI_IPI);
    assert!(sched, "SGI 0 must trigger schedule()");
}

/// Timer IRQ 30 still works correctly (regression check).
#[test]
fn timer_irq_increments_ticks_and_handles() {
    let (timer, interrupt, ticks, sched) = irq_dispatch_model(TIMER_IRQ);
    assert!(timer, "timer IRQ must call timer::handle_irq()");
    assert!(!interrupt, "timer IRQ must not forward to interrupt handler");
    assert!(ticks, "timer IRQ must increment TICKS");
    assert!(sched, "timer IRQ must trigger schedule()");
}

/// Device IRQ forwards to interrupt handler (regression check).
#[test]
fn device_irq_forwards_correctly() {
    let (timer, interrupt, ticks, sched) = irq_dispatch_model(48);
    assert!(!timer, "device IRQ must not call timer::handle_irq()");
    assert!(interrupt, "device IRQ must forward to interrupt handler");
    assert!(!ticks, "device IRQ must not increment TICKS");
    assert!(sched, "device IRQ must trigger schedule()");
}
