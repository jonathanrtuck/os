/*
 * Kernel linker script for QEMU virt (aarch64), 16 KiB granule.
 *
 * Physical memory starts at 0x4000_0000. The kernel loads at 0x4008_0000
 * (512 KiB in, leaving room for DTB at RAM base).
 *
 * Section boundaries are 16 KiB-aligned so each region can have distinct
 * page table permissions: text (RX), rodata (RO), data/bss/stack (RW).
 */

ENTRY(_start)

PHYS_BASE = 0x40080000; /* platform.rs sync: KERNEL_BASE */

SECTIONS
{
    . = PHYS_BASE;

    /* Executable code: boot trampoline + kernel text (RX). */
    __text_start = .;
    .text.boot : {
        KEEP(*(.text.boot))
    }
    .text : {
        *(.text .text.*)
    }
    __text_end = .;

    /* Read-only data (RO, no execute). */
    . = ALIGN(16384);
    __rodata_start = .;
    .rodata : {
        *(.rodata .rodata.*)
    }
    __rodata_end = .;

    /* Read-write data (RW, no execute). */
    . = ALIGN(16384);
    __data_start = .;
    .data : {
        *(.data .data.*)
    }

    /* Zero-initialized data (same RW permissions as .data). */
    . = ALIGN(16);
    __bss_start = .;
    .bss : {
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
