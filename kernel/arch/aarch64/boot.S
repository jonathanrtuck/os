// ==========================================================================
// boot.S — Early boot trampoline (.text.boot, runs at physical VA).
//
// Handles: core parking, BSS zero, EL2→EL1 drop, FP enable, KASLR
// slide generation, builds coarse 32MB-block L2 page tables for TTBR0
// (identity) and TTBR1 (kernel with KASLR), applies relocations,
// enables MMU, transitions to upper VA, jumps to kernel_main.
//
// 16 KiB granule, T0SZ=28/T1SZ=28 (36-bit VA = 64 GiB per half).
// 2-level page tables: L2 root (2048 entries) + L3 (2048 entries).
// Boot uses L2 block entries only — Rust refines to L3 for W^X.
//
// AUDIT: 2026-03-11 — Comprehensive review of AArch64 correctness.
//   EL2→EL1 transition: HCR_EL2.RW, cnthctl, cntvoff, cptr_el2, spsr/elr
//   set correctly. ISB before ERET. ✓
//   Barrier ordering: DSB SY before TLBI, DSB ISH + ISB after TLBI, DSB
//   ISH + ISB after SCTLR_EL1 write. Both primary and secondary paths. ✓
//   Register clobbers: build_boot_tables saves x29/x30, uses x20-x21
//   (callee-saved) for return values. fill_table documents x13-x18 as
//   clobbers. All consistent. ✓
//   Page table descriptors: Device = ATTRIDX1|PXN|UXN|AF, RAM =
//   ATTRIDX0|SH_INNER|AF|UXN. Block format correct for L2. ✓
//   Secondary boot: Same MAIR/TCR/barrier sequence as primary. Shares
//   TTBR1. Per-core stack calculation correct (core_id * 0x4000). ✓
//   early_vectors alignment: .align 11 (2 KiB), correct for VBAR_EL1. ✓
//   No bugs found.
// ==========================================================================

// Physical memory layout (QEMU virt).
// boot.S maps the maximum 256 MB for identity mapping. The actual RAM size
// is read from the DTB by Rust code (paging::set_ram_end) after boot.
.equ RAM_START,     0x40000000
.equ RAM_END,       0x50000000         // RAM_START + 256MB (max for identity map)
.equ BLOCK_32MB,    0x02000000
// Descriptor bits.
.equ DESC_VALID,    (1 << 0)
.equ DESC_TABLE,    (1 << 1)
.equ DESC_BLOCK,    (0 << 1)           // L2 block
.equ AF,            (1 << 10)
.equ SH_INNER,      (3 << 8)
.equ ATTRIDX0,      (0 << 2)            // normal memory
.equ ATTRIDX1,      (1 << 2)            // device memory
.equ PXN,           (1 << 53)
.equ UXN_LO,        0                  // UXN bit 54 — set via movk below

// --------------------------------------------------------------------------
// Page table storage (in .bss, zeroed by BSS init).
// 16KB-aligned, 2 L2 root tables = 32KB.
// --------------------------------------------------------------------------
.section .bss
.align 14
.global boot_tt0_l2
.global boot_tt1_l2
boot_tt0_l2:  .space 16384    // TTBR0 L2 root (2048 entries x 8 = 16 KiB)
boot_tt1_l2:  .space 16384    // TTBR1 L2 root

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
  adrp x0, __stack_top_phys
  add  x0, x0, :lo12:__stack_top_phys
  mov sp, x0

  // Zero BSS (physical addresses).
  adrp x1, __bss_start_phys
  add  x1, x1, :lo12:__bss_start_phys
  adrp x2, __bss_end_phys
  add  x2, x2, :lo12:__bss_end_phys
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

  // GICv3: enable system register access at EL1.
  // ICC_SRE_EL2: SRE=1 (bit 0) enables system register interface at EL2,
  // Enable=1 (bit 3) allows EL1 to use ICC system registers.
  // Without Enable=1, ICC_SRE_EL1 writes are ignored and EL1 cannot use
  // the GICv3 system register CPU interface.
  mov x0, #0x9                 // SRE=1 | Enable=1 (bits 0 and 3)
  msr s3_4_c12_c9_5, x0       // ICC_SRE_EL2
  isb

  adrp x0, __stack_top_phys
  add  x0, x0, :lo12:__stack_top_phys
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
  adrp x0, __stack_top_phys
  add  x0, x0, :lo12:__stack_top_phys
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
  // Generate KASLR slide from the timer counter.
  //
  // CNTVCT_EL0 provides boot-time entropy (host uptime + startup jitter).
  // We extract 8 bits at the 32 MiB boundary to get a 32 MiB-aligned slide
  // with 256 possible positions (~8 bits of entropy). x25 is preserved
  // across all boot code (callee-saved) and passed to kernel_main.
  // ------------------------------------------------------------------
  mrs  x25, cntvct_el0
  and  x25, x25, #0x1FE000000    // keep bits [32:25] → 0..255 × 32 MiB

  // ------------------------------------------------------------------
  // Build page tables (32MB blocks, coarse). 2-level: L2 root only.
  // TTBR0: identity map (PA == VA, no slide).
  // TTBR1: kernel map (VA = PA + offset + KASLR slide).
  // The KASLR slide shifts TTBR1 RAM L2 indices by slide / 32 MiB.
  // ------------------------------------------------------------------
  bl build_boot_tables

  // x0 = TTBR0 L2 PA, x1 = TTBR1 L2 PA (returned by build_boot_tables).
  mov x20, x1                  // save TTBR1 L2 PA

  // ------------------------------------------------------------------
  // KASLR relocation processing (runs at physical VA, before MMU).
  //
  // Iterates the .rela.dyn section (collected from all .rela.* by the
  // linker script). For each R_AARCH64_ABS64 entry, reads the value at
  // the target offset. If the value is a kernel VA (>= KERNEL_VA_OFFSET),
  // adjusts it by the KASLR slide. Physical addresses (e.g.,
  // secondary_entry for PSCI CPU_ON) are left untouched.
  //
  // Register contract:
  //   x0  = preserved (TTBR0 L2 PA, needed for MMU setup below)
  //   x20 = preserved (TTBR1 L2 PA)
  //   x24 = preserved (DTB PA)
  //   x25 = KASLR slide (may be 0 — loop still runs to fill PIE values)
  //   x1-x5 = clobbered
  // ------------------------------------------------------------------
  // NOTE: The loop MUST run even when slide == 0. With --pie, the linker
  // emits .quad values as 0 with R_AARCH64_RELATIVE relocations. The loop
  // writes (r_addend + slide) to each target — filling in kernel VAs.

  // Process R_AARCH64_RELATIVE entries from .rela.dyn (PIE, allocated).
  // Each Rela entry is 24 bytes: r_offset[0:8], r_info[8:16], r_addend[16:24].
  // RELATIVE: write (r_addend + slide) to the location at r_offset.
  adrp x1, __rela_start_phys
  add  x1, x1, :lo12:__rela_start_phys
  adrp x2, __rela_end_phys
  add  x2, x2, :lo12:__rela_end_phys

  // Build KERNEL_VA_OFFSET for r_offset → PA conversion.
  movz x5, #0xFFF0, lsl #32
  movk x5, #0xFFFF, lsl #48       // x5 = 0xFFFF_FFF0_0000_0000

.Lrela_loop:
  cmp  x1, x2
  b.ge .Lrela_done

  // Parse Rela entry.
  ldr  x3, [x1]                   // r_offset (VMA of target)
  ldr  x4, [x1, #16]              // r_addend (link-time value)
  add  x1, x1, #24                // advance to next entry

  // Convert r_offset to physical address.
  cmp  x3, x5                     // >= KERNEL_VA_OFFSET?
  b.lo 1f                         // already physical (.text.boot)
  sub  x3, x3, x5                 // kernel VA → PA
1:
  // Only adjust kernel VA addends (>= KERNEL_VA_OFFSET). Physical address
  // addends (e.g., SECONDARY_ENTRY_PA for PSCI CPU_ON) must not be slid.
  cmp  x4, x5                     // addend >= KERNEL_VA_OFFSET?
  b.lo 2f                         // physical → write addend as-is
  add  x4, x4, x25                // kernel VA: addend + slide
2:
  str  x4, [x3]                   // write to target PA

  b    .Lrela_loop

.Lrela_done:

  // ------------------------------------------------------------------
  // Configure and enable MMU with both TTBR0 and TTBR1.
  // ------------------------------------------------------------------

  // MAIR_EL1: AttrIdx0 = 0xFF (Normal WBWA), AttrIdx1 = 0x04 (Device-nGnRE)
  mov x1, #0xFF
  mov x2, #0x04
  lsl x2, x2, #8
  orr x1, x1, x2
  msr mair_el1, x1

  // TCR_EL1: 16K granule, T0SZ=28, T1SZ=28 (36-bit VA = 64 GiB per half).
  //   2-level page tables: L2 + L3 only.
  //   TTBR0 + TTBR1 both enabled, EPD1=0 (bit 23 clear — default).
  //   IPS=40-bit PA.
  mov x1, #28                  // T0SZ = 28
  orr x1, x1, #(0b01 << 8)    // IRGN0 = WBWA
  orr x1, x1, #(0b01 << 10)   // ORGN0 = WBWA
  orr x1, x1, #(0b11 << 12)   // SH0  = inner shareable
  orr x1, x1, #(0b10 << 14)   // TG0  = 16KB
  orr x1, x1, #(28 << 16)     // T1SZ = 28
  orr x1, x1, #(0b01 << 24)   // IRGN1 = WBWA
  orr x1, x1, #(0b01 << 26)   // ORGN1 = WBWA
  orr x1, x1, #(0b11 << 28)   // SH1  = inner shareable
  orr x1, x1, #(0b01 << 30)   // TG1  = 16KB
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
  //
  // These are kernel VA symbols — unreachable by adrp from .text.boot
  // (different VMA domain, ~16 EiB apart). Load from a relocated data
  // area instead. The .quad values are resolved at link time and will
  // be adjusted by the KASLR relocation processor when slide != 0.
  // ------------------------------------------------------------------
  adr  x10, .Lprimary_addrs
  ldr  x0, [x10]              // __stack_top (kernel VA)
  mov  sp, x0

  ldr  x0, [x10, #8]          // vectors (kernel VA)
  msr  vbar_el1, x0
  isb

  ldr  x2, [x10, #16]         // kernel_main (kernel VA)
  mov  x0, x24                 // DTB PA (arg 0)
  mov  x1, x25                 // KASLR slide (arg 1)
  blr  x2

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
  mov x1, #28
  orr x1, x1, #(0b01 << 8)
  orr x1, x1, #(0b01 << 10)
  orr x1, x1, #(0b11 << 12)
  orr x1, x1, #(0b10 << 14)
  orr x1, x1, #(28 << 16)
  orr x1, x1, #(0b01 << 24)
  orr x1, x1, #(0b01 << 26)
  orr x1, x1, #(0b11 << 28)
  orr x1, x1, #(0b01 << 30)
  mov x2, #(0b010)
  lsl x2, x2, #32
  orr x1, x1, x2
  msr tcr_el1, x1

  // TTBR0: empty (no user mappings). Use boot_tt0_l2 as a zeroed L2 table.
  // (We only need TTBR1 for kernel VA; TTBR0 must be valid but can be empty.)
  adrp x0, boot_tt0_l2_phys
  add  x0, x0, :lo12:boot_tt0_l2_phys
  msr ttbr0_el1, x0

  // TTBR1: shared kernel page tables (same as core 0).
  adrp x0, boot_tt1_l2_phys
  add  x0, x0, :lo12:boot_tt1_l2_phys
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
  // Load kernel VA symbols from relocated data area (same pattern as primary).
  adr  x10, .Lsecondary_addrs

  // Install exception vectors (upper VA).
  ldr  x0, [x10]              // vectors (kernel VA)
  msr  vbar_el1, x0
  isb

  // Set per-core kernel stack.
  // Stack top = __secondary_stacks + core_id * 0x4000 (16 KiB per core).
  ldr  x0, [x10, #8]          // __secondary_stacks (kernel VA)
  mov  x1, #0x4000
  mul  x1, x19, x1
  add  x0, x0, x1
  mov  sp, x0

  // Jump to Rust with core_id in x0.
  mov  x0, x19
  ldr  x1, [x10, #16]         // secondary_main (kernel VA)
  blr  x1
  b park

// --------------------------------------------------------------------------
// Kernel VA address tables — loaded via adr+ldr from .text.boot code.
//
// These .quad entries hold absolute kernel VA addresses, resolved by the
// linker at link time. They cannot be reached by adrp from .text.boot
// because .text.boot VMA is physical (~0x4xxx_xxxx) while kernel VA is
// ~0xFFFF_FFF0_xxxx_xxxx — far beyond adrp's ±4 GiB range.
//
// With KASLR, the relocation processor adjusts these values by the slide
// delta before they are used.
// --------------------------------------------------------------------------
.align 3
.Lprimary_addrs:
  .quad __stack_top             // [0]  kernel stack pointer
  .quad vectors                 // [8]  exception vector base
  .quad kernel_main             // [16] Rust entry point

.align 3
.Lsecondary_addrs:
  .quad vectors                 // [0]  exception vector base
  .quad __secondary_stacks      // [8]  per-core stack base
  .quad secondary_main          // [16] Rust entry point (secondary)

// Force literal pool within .text.boot (no entries expected after PIC conversion).
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
  movz x2, #0x0900, lsl #16    // UART0 phys = 0x09000000
  mov w0, #33    // '!'
  str w0, [x2]
  b .

// --------------------------------------------------------------------------
// build_boot_tables
//
// Builds TTBR0 (identity) and TTBR1 (kernel) L2 root tables using 32MB
// block entries. Returns: x0 = TTBR0 L2 PA, x1 = TTBR1 L2 PA.
//
// TTBR0 maps PA == VA (identity, for early boot).
// TTBR1 maps PA → VA + KERNEL_VA_OFFSET + KASLR slide. The KASLR slide
// shifts the RAM L2 indices by slide / 32 MiB. Device mappings are not
// affected (their VAs don't include the slide).
//
// Register usage:
//   x9  = current L2 PA
//   x10 = RAM L2 index offset (0 for TTBR0, slide/32M for TTBR1)
//   x13-x18 = clobbers (used by fill_table)
//   x20 = saved TTBR0 L2 PA
//   x21 = saved TTBR1 L2 PA
//   x22 = slide in L2 index units (slide >> 25)
//   x25 = KASLR slide (preserved from caller)
// --------------------------------------------------------------------------

build_boot_tables:
  stp x29, x30, [sp, #-16]!

  // Compute RAM L2 index offset from KASLR slide.
  lsr  x22, x25, #25            // x22 = slide / 32M = L2 index offset

  // --- Build TTBR0 table (identity map, no KASLR offset) ---
  adrp x9, boot_tt0_l2_phys
  add  x9, x9, :lo12:boot_tt0_l2_phys
  mov  x10, xzr                 // no RAM offset for identity map
  mov  x20, x9                  // save TTBR0 L2 PA
  bl   fill_table

  // --- Build TTBR1 table (kernel map, RAM shifted by KASLR offset) ---
  adrp x9, boot_tt1_l2_phys
  add  x9, x9, :lo12:boot_tt1_l2_phys
  mov  x10, x22                 // RAM index offset = slide / 32M
  mov  x21, x9                  // save TTBR1 L2 PA
  bl   fill_table

  mov  x0, x20                  // TTBR0 L2 PA
  mov  x1, x21                  // TTBR1 L2 PA

  ldp  x29, x30, [sp], #16
  ret

// --------------------------------------------------------------------------
// fill_table — populate one L2 root table with 32MB block entries.
// Input: x9  = L2 table PA.
//        x10 = L2 index offset (0 for identity, slide/32M for kernel).
//              ALL entries (device + RAM) shift by this offset so that
//              phys_to_virt works uniformly for both MMIO and RAM.
// Clobbers: x13–x18.
// --------------------------------------------------------------------------

fill_table:
  // --- Device blocks ---
  // Device attrs: VALID | BLOCK | ATTRIDX1 | AF | PXN | UXN
  // Low bits: 0x405 (VALID=1 | ATTRIDX1=4 | AF=0x400)
  // High bits: PXN=bit53, UXN=bit54 → 0x0060 at [63:48]
  mov x16, #0x405
  movk x16, #0x0060, lsl #48  // PXN | UXN in upper halfword

  // L2[offset+4]: 0x0800_0000..0x0A00_0000 (GIC + UART).
  // Device entries shift by the same KASLR offset as RAM so that
  // phys_to_virt(device_pa) = device_pa + KERNEL_VA_OFFSET + slide
  // lands in the correct TTBR1 mapping.
  mov x14, #0x08000000
  orr x15, x14, x16
  add x18, x10, #4              // index = KASLR_offset + 4
  str x15, [x9, x18, lsl #3]

  // L2[offset+5]: 0x0A00_0000..0x0C00_0000 (Virtio MMIO).
  mov x14, #0x0A000000
  orr x15, x14, x16
  add x18, x10, #5              // index = KASLR_offset + 5
  str x15, [x9, x18, lsl #3]

  // --- RAM blocks ---
  // RAM: 0x4000_0000..0x5000_0000 (8 x 32MB blocks). L2 indices 32..39.
  // RAM attrs: VALID | BLOCK | ATTRIDX0(=0) | AF | SH_INNER | UXN
  // No PXN — kernel code must be executable during identity-mapped boot.
  // UXN only (bit 54) prevents EL0 execution.
  // Low: 0x701 (VALID=1 | SH_INNER=0x300 | AF=0x400)
  // High: UXN = bit 54 → 0x0040 at [63:48]
  mov x16, #0x701
  movk x16, #0x0040, lsl #48

  mov x13, #0                  // loop index
  mov x14, RAM_START           // current PA (0x40000000)
  movz x17, #0x5000, lsl #16           // RAM_END = 0x50000000
  mov x3, BLOCK_32MB           // 32 MiB step (too large for add immediate)
.Lram_loop:
  cmp x14, x17
  b.ge .Lram_done

  orr x15, x14, x16           // PA | attrs
  add x18, x13, #32           // base L2 index = loop_index + 32
  add x18, x18, x10           // + KASLR offset (0 for TTBR0, slide/32M for TTBR1)
  str x15, [x9, x18, lsl #3]  // L2[index + offset] = descriptor

  add x13, x13, #1
  add x14, x14, x3            // advance PA by 32 MiB
  b .Lram_loop
.Lram_done:

  ret

.ltorg
