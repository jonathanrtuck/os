/*
 * Kernel linker script for QEMU virt (aarch64), 16 KiB granule.
 *
 * Physical memory starts at 0x4000_0000. The kernel loads at 0x4008_0000
 * (512 KiB in, leaving room for DTB at RAM base).
 *
 * The kernel runs from the TTBR1 upper-half address space. All sections
 * have VMA in the upper half. LMA = VMA - VA_OFFSET, so the ELF loader
 * places bytes at the physical address and the MMU maps them at the VA.
 *
 * .text.boot is identity-mapped in TTBR0 so it can survive the MMU
 * enable transition (the instruction after MSR SCTLR_EL1 must resolve
 * at the physical address).
 *
 * Section boundaries are 16 KiB-aligned so each region can have distinct
 * page table permissions: text (RX), rodata (RO), data/bss/stack (RW).
 *
 * platform.rs sync: KERNEL_BASE, KERNEL_VA_OFFSET
 */

ENTRY(_start)

PHYS_BASE  = 0x40080000;
VA_OFFSET  = 0xFFFFFFF000000000;
VIRT_BASE  = PHYS_BASE + VA_OFFSET;

SECTIONS
{
    . = VIRT_BASE;

    /* Boot trampoline: identity-mapped in TTBR0 at the physical address. */
    __text_start = .;
    .text.boot : AT(ADDR(.text.boot) - VA_OFFSET) {
        KEEP(*(.text.boot))
    }

    /* Executable code (RX). */
    .text : AT(ADDR(.text) - VA_OFFSET) {
        *(.text .text.*)
    }
    __text_end = .;

    /* Read-only data (RO, no execute). */
    . = ALIGN(16384);
    __rodata_start = .;
    .rodata : AT(ADDR(.rodata) - VA_OFFSET) {
        *(.rodata .rodata.*)
    }
    __rodata_end = .;

    /* Read-write data (RW, no execute). */
    . = ALIGN(16384);
    __data_start = .;
    .data : AT(ADDR(.data) - VA_OFFSET) {
        *(.data .data.*)
    }

    /* Zero-initialized data (same RW permissions as .data). */
    . = ALIGN(16384);
    __bss_start = .;
    .bss : AT(ADDR(.bss) - VA_OFFSET) {
        *(.bss .bss.*)
        *(COMMON)
    }
    __bss_end = .;

    /* BSP early-boot stack (64 KiB). Used by boot.S before Rust.
     * Per-core stacks for all cores live in CORE_STACKS (cpu.rs, in .bss). */
    . = ALIGN(16);
    .bss.stack (NOLOAD) : {
        __stack_bottom = .;
        . = . + 0x10000; /* config.rs sync: KERNEL_STACK_SIZE */
        __stack_top = .;
    }

    /* Discard unneeded sections. */
    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment .note.*)
    }

    . = ALIGN(16384);
    __kernel_end = .;
}
