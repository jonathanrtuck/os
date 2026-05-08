/*
 * Service linker script — userspace binary at standard VA base.
 * All services share the same layout since each runs in its own address space.
 */

ENTRY(_start)

SERVICE_VA_BASE = 0x00200000;

SECTIONS
{
    . = SERVICE_VA_BASE;

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
