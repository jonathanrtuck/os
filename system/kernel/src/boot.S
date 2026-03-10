// ==========================================================================
// boot.S — Early boot trampoline (.text.boot, runs at physical VA).
//
// Handles: core parking, BSS zero, EL2→EL1 drop, FP enable, builds
// coarse 2MB-block page tables for TTBR0 (identity) and TTBR1 (kernel),
// enables MMU, transitions to upper VA, jumps to kernel_main.
//
// After kernel_main starts (at upper VA via TTBR1), Rust code refines
// TTBR1 with 4KB pages for W^X enforcement.
// ==========================================================================

// Physical memory layout (QEMU virt, 256MB RAM).
.equ RAM_START,     0x40000000
.equ RAM_END,       0x50000000         // RAM_START + 256MB
.equ BLOCK_2MB,     0x00200000
// Descriptor bits.
.equ DESC_VALID,    (1 << 0)
.equ DESC_TABLE,    (1 << 1)
.equ DESC_BLOCK,    (0 << 1)           // L1/L2 block
.equ AF,            (1 << 10)
.equ SH_INNER,      (3 << 8)
.equ ATTRIDX0,      (0 << 2)            // normal memory
.equ ATTRIDX1,      (1 << 2)            // device memory
.equ PXN,           (1 << 53)
.equ UXN_LO,        0                  // UXN bit 54 — set via movk below
// KERNEL_VA_OFFSET = 0xFFFF_0000_0000_0000
.equ KVA_OFFSET_HI, 0xFFFF0000         // upper 16 bits in [31:16] position

// VA → PA: clear bits [63:48] (kernel VA offset). Used during early boot
// when we need physical addresses from linker symbols (which are upper VA).
.macro va_to_pa reg
  and \reg, \reg, #0x0000FFFFFFFFFFFF
.endm

// --------------------------------------------------------------------------
// Page table storage (in .bss, zeroed by BSS init).
// 4KB-aligned, 6 tables = 24KB.
// --------------------------------------------------------------------------
.section .bss
.align 12
.global boot_tt0_l0
.global boot_tt0_l1
.global boot_tt0_l2_0
.global boot_tt0_l2_1
.global boot_tt1_l2_1
boot_tt0_l0:    .space 4096    // TTBR0 L0
boot_tt0_l1:    .space 4096    // TTBR0 L1
boot_tt0_l2_0:  .space 4096    // TTBR0 L2 for 0..1GB (devices)
boot_tt0_l2_1:  .space 4096    // TTBR0 L2 for 1..2GB (RAM)
boot_tt1_l0:    .space 4096    // TTBR1 L0
boot_tt1_l1:    .space 4096    // TTBR1 L1
boot_tt1_l2_0:  .space 4096    // TTBR1 L2 for 0..1GB (devices)
boot_tt1_l2_1:  .space 4096    // TTBR1 L2 for 1..2GB (RAM)

// --------------------------------------------------------------------------
// Boot entry (.text.boot — physical VMA)
// --------------------------------------------------------------------------

.section .text.boot
.global _start
_start:
  // Save DTB physical address (firmware passes in x0 per aarch64 boot protocol).
  // x24 is callee-saved and not used by any boot-time routine.
  mov x24, x0

  // Park secondary cores.
  mrs x0, mpidr_el1
  and x0, x0, #0xFF
  cbnz x0, park

  // Physical stack for early boot.
  ldr x0, =__stack_top_phys
  mov sp, x0

  // Zero BSS (physical addresses).
  ldr x1, =__bss_start_phys
  ldr x2, =__bss_end_phys
1:
  cmp x1, x2
  b.ge 2f
  str xzr, [x1], #8
  b 1b
2:
  // If entered at EL2, drop to EL1.
  mrs x1, CurrentEL
  lsr x1, x1, #2
  and x1, x1, #3
  cmp x1, #2
  b.ne el1_start

  // --- EL2 → EL1 ---
  mov x0, #(1 << 31)           // HCR_EL2.RW = 1
  msr hcr_el2, x0

  mrs x0, cnthctl_el2
  orr x0, x0, #3
  msr cnthctl_el2, x0
  msr cntvoff_el2, xzr

  mrs x0, cptr_el2
  bic x0, x0, #(1 << 10)       // TFP = 0
  msr cptr_el2, x0

  ldr x0, =__stack_top_phys
  msr sp_el1, x0
  mov x0, #0b0101              // EL1h
  msr spsr_el2, x0
  adr x0, el1_start
  msr elr_el2, x0
  isb
  eret

el1_start:
  msr spsel, #1
  isb
  ldr x0, =__stack_top_phys
  mov sp, x0

  // Install early exception vectors (physical VA, in .text.boot).
  // These catch faults during MMU bring-up; replaced by upper VA vectors later.
  adr x0, early_vectors
  msr vbar_el1, x0
  isb

  // FP/ASIMD enable.
  mrs x0, cpacr_el1
  orr x0, x0, #(3 << 20)
  msr cpacr_el1, x0
  isb

  // ------------------------------------------------------------------
  // Build page tables (all 2MB blocks, coarse).
  // TTBR0: identity map (PA == VA). TTBR1: kernel map (VA = PA + offset,
  // but page table indices are identical since offset only affects [63:48]).
  // ------------------------------------------------------------------
  bl build_boot_tables

  // x0 = TTBR0 L0 PA, x1 = TTBR1 L0 PA (returned by build_boot_tables).
  mov x20, x1                  // save TTBR1 L0 PA

  // ------------------------------------------------------------------
  // Configure and enable MMU with both TTBR0 and TTBR1.
  // ------------------------------------------------------------------

  // MAIR_EL1: AttrIdx0 = 0xFF (Normal WBWA), AttrIdx1 = 0x04 (Device-nGnRE)
  mov x1, #0xFF
  mov x2, #0x04
  lsl x2, x2, #8
  orr x1, x1, x2
  msr mair_el1, x1

  // TCR_EL1: TTBR0 + TTBR1 both enabled.
  //   T0SZ=16, TG0=4k, SH0=inner, ORGN0=WBWA, IRGN0=WBWA
  //   T1SZ=16, TG1=4k, SH1=inner, ORGN1=WBWA, IRGN1=WBWA
  //   EPD1=0 (TTBR1 walks enabled)
  //   IPS=40-bit PA
  mov x1, #16                  // T0SZ = 16
  orr x1, x1, #(0b11 << 12)   // SH0  = inner shareable
  orr x1, x1, #(0b01 << 10)   // ORGN0 = WBWA
  orr x1, x1, #(0b01 << 8)    // IRGN0 = WBWA
  // EPD1 = 0 (bit 23 clear — default)
  orr x1, x1, #(16 << 16)     // T1SZ = 16
  orr x1, x1, #(0b01 << 24)   // IRGN1 = WBWA
  orr x1, x1, #(0b01 << 26)   // ORGN1 = WBWA
  orr x1, x1, #(0b11 << 28)   // SH1  = inner shareable
  orr x1, x1, #(0b10 << 30)   // TG1  = 4KB
  mov x2, #(0b010)             // IPS = 40-bit
  lsl x2, x2, #32
  orr x1, x1, x2
  msr tcr_el1, x1

  msr ttbr0_el1, x0
  msr ttbr1_el1, x20

  dsb sy
  isb
  tlbi vmalle1
  dsb ish
  isb

  mrs x1, sctlr_el1
  orr x1, x1, #1              // M = 1 (enable MMU)
  orr x1, x1, #(1 << 2)       // C = 1 (enable D-cache)
  orr x1, x1, #(1 << 12)      // I = 1 (enable I-cache)
  msr sctlr_el1, x1
  dsb ish
  isb

  // ------------------------------------------------------------------
  // MMU is live. Both TTBR0 (identity) and TTBR1 (kernel) active.
  // Switch SP to upper VA, install vectors, jump to kernel_main.
  // ------------------------------------------------------------------
  ldr x0, =__stack_top
  mov sp, x0

  ldr x0, =vectors
  msr vbar_el1, x0
  isb

  mov x0, x24              // DTB PA as first argument to kernel_main
  ldr x1, =kernel_main
  blr x1

park:
  wfe
  b park

// --------------------------------------------------------------------------
// secondary_entry — Trampoline for secondary cores (called via PSCI CPU_ON).
//
// Entered at EL1 (PSCI drops from EL2). TTBR1 page tables are already live
// from core 0's boot — we just need to configure this core's MMU, set up
// its stack, and jump to Rust.
//
// Input: x0 = context_id (core_id, 1..7).
// --------------------------------------------------------------------------

.global secondary_entry
secondary_entry:
  // x0 = core_id (passed by PSCI CPU_ON as context_id). Save it.
  mov x19, x0

  // FP/ASIMD enable.
  mrs x0, cpacr_el1
  orr x0, x0, #(3 << 20)
  msr cpacr_el1, x0
  isb

  // MAIR_EL1 — same as core 0.
  mov x1, #0xFF
  mov x2, #0x04
  lsl x2, x2, #8
  orr x1, x1, x2
  msr mair_el1, x1

  // TCR_EL1 — same as core 0.
  mov x1, #16
  orr x1, x1, #(0b11 << 12)
  orr x1, x1, #(0b01 << 10)
  orr x1, x1, #(0b01 << 8)
  orr x1, x1, #(16 << 16)
  orr x1, x1, #(0b01 << 24)
  orr x1, x1, #(0b01 << 26)
  orr x1, x1, #(0b11 << 28)
  orr x1, x1, #(0b10 << 30)
  mov x2, #(0b010)
  lsl x2, x2, #32
  orr x1, x1, x2
  msr tcr_el1, x1

  // TTBR0: empty (no user mappings). Use boot_tt0_l0 as a zeroed L0 table.
  // (We only need TTBR1 for kernel VA; TTBR0 must be valid but can be empty.)
  ldr x0, =boot_tt0_l0
  va_to_pa x0
  msr ttbr0_el1, x0

  // TTBR1: shared kernel page tables (same as core 0).
  ldr x0, =boot_tt1_l0
  va_to_pa x0
  msr ttbr1_el1, x0

  dsb sy
  isb
  tlbi vmalle1
  dsb ish
  isb

  // Enable MMU + caches.
  mrs x1, sctlr_el1
  orr x1, x1, #1              // M = 1
  orr x1, x1, #(1 << 2)       // C = 1
  orr x1, x1, #(1 << 12)      // I = 1
  msr sctlr_el1, x1
  dsb ish
  isb

  // --- MMU is live. Switch to upper VA. ---

  // Install exception vectors (upper VA).
  ldr x0, =vectors
  msr vbar_el1, x0
  isb

  // Set per-core kernel stack.
  // Stack top = __secondary_stacks + core_id * 0x4000 (16 KiB per core).
  ldr x0, =__secondary_stacks
  mov x1, #0x4000
  mul x1, x19, x1
  add x0, x0, x1
  mov sp, x0

  // Jump to Rust with core_id in x0.
  mov x0, x19
  ldr x1, =secondary_main
  blr x1
  b park

// Force literal pool within .text.boot (physical VMA).
.ltorg

// --------------------------------------------------------------------------
// Store secondary_entry's physical address in .rodata (upper VA, reachable
// from Rust). .quad uses R_AARCH64_ABS64, which has no range limit.
// Rust reads this value to pass to PSCI CPU_ON.
// --------------------------------------------------------------------------
.section .rodata
.align 3
.global SECONDARY_ENTRY_PA
SECONDARY_ENTRY_PA:
  .quad secondary_entry

// Restore to .text.boot for the rest of boot-time code.
.section .text.boot

// --------------------------------------------------------------------------
// Early exception vectors (physical VA, used during MMU bring-up).
// Prints '!' to UART then hangs — any exception here is fatal.
// --------------------------------------------------------------------------

.align 11
early_vectors:
  b early_exc     // 0x000
  .space 0x80 - 4
  b early_exc     // 0x080
  .space 0x80 - 4
  b early_exc     // 0x100
  .space 0x80 - 4
  b early_exc     // 0x180
  .space 0x80 - 4
  b early_exc     // 0x200
  .space 0x80 - 4
  b early_exc     // 0x280
  .space 0x80 - 4
  b early_exc     // 0x300
  .space 0x80 - 4
  b early_exc     // 0x380
  .space 0x80 - 4
  b early_exc     // 0x400
  .space 0x80 - 4
  b early_exc     // 0x480
  .space 0x80 - 4
  b early_exc     // 0x500
  .space 0x80 - 4
  b early_exc     // 0x580
  .space 0x80 - 4
  b early_exc     // 0x600
  .space 0x80 - 4
  b early_exc     // 0x680
  .space 0x80 - 4
  b early_exc     // 0x700
  .space 0x80 - 4
  b early_exc     // 0x780
  .space 0x80 - 4

early_exc:
  ldr x2, =0x09000000
  mov w0, #33    // '!'
  str w0, [x2]
  b .

// --------------------------------------------------------------------------
// build_boot_tables
//
// Builds TTBR0 (identity) and TTBR1 (kernel) page tables using 2MB blocks.
// Returns: x0 = TTBR0 L0 PA, x1 = TTBR1 L0 PA.
//
// Both table sets have identical structure (same indices, same PAs) because
// the kernel VA offset only affects bits [63:48], not the index bits.
//
// Register usage:
//   x9  = current L0 PA
//   x10 = current L1 PA
//   x11 = current L2_0 PA (devices)
//   x12 = current L2_1 PA (RAM)
//   x13 = loop counter
//   x14 = current PA
//   x15 = descriptor value
//   x20 = saved TTBR0 L0 PA
//   x21 = saved TTBR1 L0 PA
// --------------------------------------------------------------------------

build_boot_tables:
  stp x29, x30, [sp, #-16]!

  // --- Build TTBR0 tables (identity map) ---
  ldr x9,  =boot_tt0_l0
  ldr x10, =boot_tt0_l1
  ldr x11, =boot_tt0_l2_0
  ldr x12, =boot_tt0_l2_1

  // These are upper VA symbols; convert to PA.
  // PA = VA - 0xFFFF_0000_0000_0000
  // Since the offset only sets bits [63:48], clearing those bits gives PA.
  va_to_pa x9
  va_to_pa x10
  va_to_pa x11
  va_to_pa x12

  mov x20, x9                  // save TTBR0 L0 PA
  bl fill_tables

  // --- Build TTBR1 tables (kernel map) ---
  ldr x9,  =boot_tt1_l0
  ldr x10, =boot_tt1_l1
  ldr x11, =boot_tt1_l2_0
  ldr x12, =boot_tt1_l2_1

  va_to_pa x9
  va_to_pa x10
  va_to_pa x11
  va_to_pa x12

  mov x21, x9                  // save TTBR1 L0 PA
  bl fill_tables

  mov x0, x20                  // TTBR0 L0 PA
  mov x1, x21                  // TTBR1 L0 PA

  ldp x29, x30, [sp], #16
  ret

// --------------------------------------------------------------------------
// fill_tables — populate one set of L0/L1/L2 tables.
// Input: x9=L0, x10=L1, x11=L2_0, x12=L2_1 (all PA).
// Clobbers: x13, x14, x15.
// --------------------------------------------------------------------------

fill_tables:
  // L0[0] → L1
  mov x15, x10
  orr x15, x15, #(DESC_VALID | DESC_TABLE)
  str x15, [x9]

  // L1[0] → L2_0 (0..1GB, devices)
  mov x15, x11
  orr x15, x15, #(DESC_VALID | DESC_TABLE)
  str x15, [x10]

  // L1[1] → L2_1 (1..2GB, RAM)
  mov x15, x12
  orr x15, x15, #(DESC_VALID | DESC_TABLE)
  str x15, [x10, #8]

  // --- Device blocks in L2_0 ---
  // Device attrs: VALID | ATTRIDX1 | AF | PXN | UXN
  // Low bits: 0x405 (VALID=1 | ATTRIDX1=4 | AF=0x400)
  // High bits: PXN=bit53, UXN=bit54 → bits [55:52] = 0x60 → word at [63:32] = 0x00600000
  mov x16, #0x405
  movk x16, #0x0060, lsl #48  // PXN | UXN in upper halfword

  // GIC: 0x0800_0000, L2 index = 0x0800_0000 >> 21 = 64
  mov x14, #0x08000000
  orr x15, x14, x16
  str x15, [x11, #(64 * 8)]

  // UART: 0x0900_0000, L2 index = 0x0900_0000 >> 21 = 72
  mov x14, #0x09000000
  orr x15, x14, x16
  str x15, [x11, #(72 * 8)]

  // Virtio MMIO: 0x0A00_0000, L2 index = 0x0A00_0000 >> 21 = 80
  mov x14, #0x0A000000
  orr x15, x14, x16
  str x15, [x11, #(80 * 8)]

  // --- RAM blocks in L2_1 ---
  // RAM: 0x4000_0000..0x5000_0000 (128 * 2MB blocks). L2_1[0..127].
  // Descriptor: PA | VALID | ATTRIDX0(=0) | AF | SH_INNER | PXN | UXN
  // Pre-compute attribute bits in x16 (PA-independent).
  // RAM attrs: VALID | ATTRIDX0(=0) | AF | SH_INNER | UXN
  // No PXN — kernel code must be executable during identity-mapped boot.
  // UXN only (bit 54) prevents EL0 execution.
  // Low: 0x701 (VALID=1 | SH_INNER=0x300 | AF=0x400)
  // High: UXN = bit 54 → 0x0040 at [63:48]
  mov x16, #0x701
  movk x16, #0x0040, lsl #48

  mov x13, #0                 // loop index
  mov x14, RAM_START           // current PA
  ldr x17, =RAM_END
.Lram_loop:
  cmp x14, x17
  b.ge .Lram_done

  orr x15, x14, x16           // PA | attrs
  str x15, [x12, x13, lsl #3]

  add x13, x13, #1
  add x14, x14, BLOCK_2MB
  b .Lram_loop
.Lram_done:

  ret

.ltorg
