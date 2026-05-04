/*
 * Integration test binary linker script.
 *
 * Must match INIT_CODE_VA in kernel/src/bootstrap.rs.
 * .text.boot is placed first so _start is at INIT_VA_BASE.
 */

ENTRY(_start)

INIT_VA_BASE = 0x00200000;

SECTIONS
{
    . = INIT_VA_BASE;

    .text : {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    . = ALIGN(16);
    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(16);
    .data : {
        *(.data .data.*)
    }

    . = ALIGN(16);
    .bss : {
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment .note.*)
    }
}
